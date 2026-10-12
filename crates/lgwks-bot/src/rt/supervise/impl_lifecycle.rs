impl Supervisor {
    /// Cancel every task, wait for the set to drain, and return what it
    /// drained.
    ///
    /// A task built on [`Supervisor::spawn_repeating`] or [`repeat`] observes
    /// the token and returns from its loop, and `repeat` yields the executor
    /// every few iterations, so a repeating loop no longer has to *suspend* to
    /// become cancellable. One that never observes its token at all is aborted,
    /// which takes effect the next time that task yields. A cooperative return
    /// is reported as [`TaskOutcome::Cancelled`] and an abort as
    /// [`TaskOutcome::Aborted`], which is the difference this return value
    /// exists to make.
    ///
    /// # Limits
    ///
    /// Abort is cooperative. A future that never returns from `poll` — a
    /// blocking call, an unbounded loop with no await, a body that ignores its
    /// token and never suspends — is not preemptible, and this call waits for
    /// it. That is a property of cooperative scheduling, not of this
    /// supervisor: nothing in a thread-per-task executor can stop a poll in
    /// flight from the outside. Such work belongs behind an explicit budget or
    /// in another process. What this module guarantees is the other direction:
    /// every loop *it* drives is bounded per poll, so cancellation and abort do
    /// land.
    pub async fn shutdown(mut self) -> ShutdownReport {
        self.token.cancel();
        // Give a cooperative body room to return on its own before the abort
        // lands, so `Cancelled` and `Aborted` mean what they say. Bounded by
        // `COOPERATIVE_DRAIN_GRACE`: this never waits on a body that ignores its
        // token.
        //
        // The loop absorbs what has finished and then yields, rather than
        // yielding a fixed number of times and only then joining: a body that
        // returns early is settled immediately, so the grace is a ceiling on
        // the wait and not a charge for it.
        // The grace is bounded on the **wall** clock, deliberately, and not on
        // the supervisor's governing clock. This loop waits for real work to
        // reach a join boundary; a caller-advanceable clock would let one
        // advance end the grace before any body ran, and the abort would then
        // report every cooperative task as `Aborted` — a false terminal state
        // produced by the test's own clock. The wall watchdog is the one bound
        // that cannot be moved by whoever is driving time.
        let watchdog = self.clock.wall_watchdog();
        let deadline = watchdog.elapsed().saturating_add(COOPERATIVE_DRAIN_GRACE);
        // The fewest live process tasks seen so far, and when that count last
        // fell: a drain finishing is the progress the process grace runs from.
        let mut fewest_live = self.live_cleanups();
        let mut progressed_at = deadline;
        loop {
            while let Some(joined) = self.set.try_join_next_with_id() {
                self.absorb(joined, Retention::Draining);
            }
            // The set emptying is the real exit condition; the grace is the
            // ceiling on how long the loop may keep trying, extended only while a
            // process task is still draining its bounded cleanup and drains are
            // still finishing.
            let elapsed = watchdog.elapsed();
            let live = self.live_cleanups();
            if live < fewest_live {
                fewest_live = live;
                progressed_at = progressed_at.max(elapsed);
            }
            if self.set.is_empty()
                || (elapsed >= deadline
                    && !self.cleanup_holds_grace(elapsed.saturating_sub(progressed_at)))
            {
                break;
            }
            yield_now().await;
        }
        // Everything still running is dropped here, which is what makes the
        // drain below terminate rather than wait for a body that will not stop.
        self.set.abort_all();
        while let Some(joined) = self.set.join_next_with_id().await {
            self.absorb(joined, Retention::Draining);
        }
        #[cfg(all(unix, feature = "process"))]
        self.refresh_cleanup_owners();
        let stats = self.stats();
        let outcomes: Vec<TaskOutcome> = self.reports.drain(..).collect();
        ShutdownReport {
            outcomes,
            stats,
            #[cfg(all(unix, feature = "process"))]
            cleanup_owners: Arc::clone(&self.cleanup_owners),
        }
    }

    /// Whether shutdown keeps waiting, `quiet` after the later of its
    /// cooperative grace and the last drain that finished: only while a process
    /// task is live and [`PROCESS_CLEANUP_GRACE`] has not passed without one.
    #[cfg(all(any(unix, windows), feature = "process"))]
    fn cleanup_holds_grace(&self, quiet: Duration) -> bool {
        quiet < PROCESS_CLEANUP_GRACE && self.live_cleanups() > 0
    }

    /// Supervised process tasks that have not yet finished their cleanup.
    #[cfg(all(unix, feature = "process"))]
    fn live_cleanups(&self) -> usize {
        self.cleanup_owners
            .live
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Supervised process tasks that have not yet settled their job.
    #[cfg(all(windows, feature = "process"))]
    fn live_cleanups(&self) -> usize {
        self.live_jobs.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Without supervised processes no task drains a cleanup.
    #[cfg(not(all(any(unix, windows), feature = "process")))]
    const fn live_cleanups(&self) -> usize {
        0
    }

    /// Without supervised processes nothing drains a cleanup, so the cooperative
    /// grace is the whole wait.
    #[cfg(not(all(any(unix, windows), feature = "process")))]
    const fn cleanup_holds_grace(&self, _quiet: Duration) -> bool {
        false
    }

    /// Take a permit, waiting for one if the bound is reached, and reap first.
    ///
    /// `None` is a refusal, already counted. A cancelled supervisor refuses
    /// before the wait, during the wait, and at the moment a permit lands:
    /// cancellation is the terminal admission state, and a slot freeing after
    /// it is not an admission offer. The pool's own fair queue holds the
    /// waiter's place, so the acquire is pinned for the whole wait and raced
    /// against the token rather than re-created: a dropped acquire future
    /// forfeits its place in that queue, which is what lets a caller that
    /// arrived later take the permit a longer-waiting caller was queued for.
    /// `run_until_cancelled` polls the acquire first on every wake, so the race
    /// between "a slot freed" and "the supervisor was cancelled" is decided for
    /// cancellation on ties.
    async fn claim(&mut self) -> Option<Lease> {
        // The gate before the wait: a cancelled supervisor never parks on a
        // full pool, because there is nothing it would do with a slot.
        if self.token.is_cancelled() {
            return self.refuse();
        }
        // With a policy installed, the untenanted calls are one implicit tenant
        // and the round — not the pool — decides who a freed permit belongs to.
        #[cfg(feature = "script")]
        if let Some(shell) = self.tenancy.clone() {
            // The untenanted path keeps its silence: a refusal counts and returns,
            // which is exactly what it did before a tenant could be named. The
            // typed refusal belongs to the tenant-scoped doors.
            return match self.claim_tenanted(&shell, &implicit_tenant()).await {
                Ok(lease) => Some(lease),
                Err(_) => {
                    self.refused = self.refused.saturating_add(1);
                    None
                }
            };
        }
        self.reap();

        // The fast path, and it is the common one: an uncontended spawn takes a
        // permit that is already free, with no future built and no timer armed.
        //
        // The slow path, and for a bounded pipeline the steady state: the pool
        // is full and every spawn waits. A permit held by a task is released
        // when that task's future finishes, which is before the task set can
        // yield it, so a full pool frees exactly when a task can be joined. The
        // wait is therefore on the task set — the same wait a raw `JoinSet` +
        // `Semaphore` pipeline makes — and the outcome is absorbed as it arrives
        // rather than on a later reap. Waiting on the semaphore instead cost an
        // extra cross-thread wake per spawn, because the permit is released from
        // inside the finishing task, before the task set has anything to give
        // (#269).
        loop {
            if let Some(permit) = self.try_take() {
                return self.admit(permit);
            }
            match self.wait_at_bound().await {
                Wait::Joined(joined) => self.absorb(joined, Retention::Capped),
                Wait::Recheck => {
                    self.reap();
                }
                Wait::Permit(permit) => return self.admit(permit),
                Wait::Closed | Wait::Cancelled => return self.refuse(),
            }
        }
    }

    /// The tenanted counterpart of [`Supervisor::claim`]: the same wait, against
    /// the round instead of the pool, and a typed refusal instead of a silent
    /// `None` so [`Supervisor::spawn_for`] can hand the caller the tenant that
    /// was at capacity.
    ///
    /// A cancelled supervisor refuses before the wait, during the wait, and at
    /// the moment a permit lands, exactly as the untenanted path does: the token
    /// is read before the wait and again on the permit. There is **no** loop
    /// here, and that is the contract rather than an omission: one arrival
    /// produces exactly one waiter, and the round's queue is FIFO, so a second
    /// attempt would not make progress faster — it would forfeit the place the
    /// first attempt earned.
    ///
    /// Reaping is bookkeeping, not admission — a lease is released inside its
    /// task the moment the body ends, never at the reap — so it happens once per
    /// call, *after* the round has decided, and joins at most
    /// `REAP_PER_ADMISSION` tasks. A queued caller reaps while its grant is on
    /// the way, so the joins cost it nothing it was not already waiting
    /// through. Reaping everything first put every task any tenant had finished
    /// on the critical path of whichever tenant called next, which is how a
    /// tenant whose bodies end at once charged its own bookkeeping to its
    /// neighbour (#268's adversarial row). A grant delivered during the reap
    /// waits in the waiter's slot, which the first poll takes.
    #[cfg(feature = "script")]
    async fn claim_tenanted(
        &mut self,
        shell: &Arc<tenancy_support::TenancyShell>,
        tenant: &Tenant,
    ) -> Result<Lease, SpawnRefused> {
        if self.token.is_cancelled() {
            let refusal = Err(SpawnRefused::Cancelled);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "claim_tenanted: returning an error to the caller");
            return refusal;
        }
        match shell.admit(tenant) {
            tenancy_support::Admission::Admitted(lease) => {
                self.reap_at_most(REAP_PER_ADMISSION);
                // Cancellation wins the race for a permit that landed in the
                // same instant, and the lease's drop returns the capacity to the
                // round either way.
                if self.token.is_cancelled() {
                    Err(SpawnRefused::Cancelled)
                } else {
                    Ok(lease)
                }
            }
            tenancy_support::Admission::Refused { limit } => Err(SpawnRefused::TenantAtCapacity {
                tenant: tenant.clone(),
                limit,
            }),
            tenancy_support::Admission::SupervisorQueueFull { limit } => {
                Err(SpawnRefused::SupervisorQueueFull { limit })
            }
            tenancy_support::Admission::Queued(waiter) => {
                // Park on **this** waiter until it resolves or the token
                // cancels. The waiter is pinned for the whole wait and never
                // re-created, because the round's queue is FIFO: dropping it to
                // re-ask forfeits the place this caller earned and leaves an
                // abandoned entry behind for every interval it waited. A grant
                // does not need a poll interval to arrive — the completion hook
                // hands the permit to this waiter and wakes it directly — and
                // `run_until_cancelled` wakes on the token too, so neither
                // direction needs a timer.
                let mut waiter = std::pin::pin!(waiter);
                self.reap_at_most(REAP_PER_ADMISSION);
                let token = self.token.clone();
                match token.run_until_cancelled(waiter.as_mut()).await {
                    None => Err(SpawnRefused::Cancelled),
                    Some(permit) if !self.token.is_cancelled() => {
                        Ok(Lease::tenanted(permit, Arc::clone(shell), tenant.clone()))
                    }
                    Some(permit) => {
                        // Cancellation won the race against a grant that had
                        // already been charged to this tenant. The permit is
                        // handed back through the round, so the tenant is not
                        // left holding an admission nobody owns.
                        drop(Lease::tenanted(permit, Arc::clone(shell), tenant.clone()));
                        Err(SpawnRefused::Cancelled)
                    }
                }
            }
        }
    }

    /// Turn a permit into an admission, unless cancellation landed first.
    ///
    /// Cancellation is read again on the permit itself, so a slot that frees
    /// after cancellation is not an admission offer: the permit drops here and
    /// the refusal is counted. Every path that obtains a permit decides through
    /// this one rule.
    fn admit(&mut self, permit: OwnedSemaphorePermit) -> Option<Lease> {
        if self.token.is_cancelled() {
            drop(permit);
            return self.refuse();
        }
        Some(Lease::plain(permit))
    }

    /// Count one refused admission.
    const fn refuse(&mut self) -> Option<Lease> {
        self.refused = self.refused.saturating_add(1);
        None
    }

    /// Wait at the bound for the next thing that can change the admission
    /// decision: a task ending, cancellation, or a cleanup owner to re-drive.
    ///
    /// With tasks in flight the wait is on the task set, because ending is how a
    /// task gives its permit back. With none in flight every permit is held
    /// elsewhere — by a cleanup owner — and the wait is on the pool itself.
    /// The futures are pinned on this frame, not boxed, and polled in the order
    /// the decision needs: the slot first, then the token, then the recheck.
    async fn wait_at_bound(&mut self) -> Wait {
        let permits = &self.permits;
        let set = &mut self.set;
        let mut cancelled = pin!(self.token.cancelled());
        #[cfg(all(unix, feature = "process"))]
        let mut recheck = pin!(recheck(&self.cleanup_owners));
        #[cfg(not(all(unix, feature = "process")))]
        let mut recheck = pin!(std::future::pending::<()>());
        if set.is_empty() {
            let mut acquire = pin!(Arc::clone(permits).acquire_owned());
            return poll_fn(|context: &mut Context<'_>| {
                if let Poll::Ready(acquired) = acquire.as_mut().poll(context) {
                    return Poll::Ready(acquired.map_or(Wait::Closed, Wait::Permit));
                }
                if cancelled.as_mut().poll(context).is_ready() {
                    return Poll::Ready(Wait::Cancelled);
                }
                recheck.as_mut().poll(context).map(|()| Wait::Recheck)
            })
            .await;
        }
        let mut joined = pin!(set.join_next_with_id());
        poll_fn(|context: &mut Context<'_>| {
            if let Poll::Ready(next) = joined.as_mut().poll(context) {
                // `None` is an empty set, which was ruled out above; reading it
                // as a recheck re-enters the loop rather than inventing a state.
                return Poll::Ready(next.map_or(Wait::Recheck, Wait::Joined));
            }
            if cancelled.as_mut().poll(context).is_ready() {
                return Poll::Ready(Wait::Cancelled);
            }
            recheck.as_mut().poll(context).map(|()| Wait::Recheck)
        })
        .await
    }

    /// The non-blocking counterpart of [`Supervisor::claim`].
    ///
    /// Cancellation is checked before the pool and again on the permit, so a
    /// cancelled supervisor refuses even when the pool has room.
    fn claim_now(&mut self) -> Option<Lease> {
        if self.token.is_cancelled() {
            self.refused = self.refused.saturating_add(1);
            return None;
        }
        self.reap();
        #[cfg(feature = "script")]
        if let Some(shell) = self.tenancy.clone() {
            return match shell.try_admit(&implicit_tenant()) {
                tenancy_support::Admission::Admitted(lease) if !self.token.is_cancelled() => {
                    Some(lease)
                }
                tenancy_support::Admission::Admitted(_) => {
                    // The lease drops here, returning the permit it just took.
                    self.refused = self.refused.saturating_add(1);
                    None
                }
                tenancy_support::Admission::Refused { .. }
                | tenancy_support::Admission::SupervisorQueueFull { .. } => {
                    self.refused = self.refused.saturating_add(1);
                    None
                }
                tenancy_support::Admission::Queued(_) => {
                    // `try_admit` never queues; a queued answer would be a defect
                    // in the door, and it is reported as the capacity refusal it
                    // must never be mistaken for.
                    self.refused = self.refused.saturating_add(1);
                    None
                }
            };
        }
        if self.token.is_cancelled() {
            // Cancellation can land inside `reap`, which awaits. The permit, if one
            // landed below, would drop here: refusal wins the race.
            self.refused = self.refused.saturating_add(1);
            return None;
        }
        match self.try_take() {
            Some(permit) => Some(Lease::plain(permit)),
            None => {
                self.refused = self.refused.saturating_add(1);
                None
            }
        }
    }

    /// Place a caller's body under `lease`, reporting the two ways a body that
    /// returned can end.
    ///
    /// One wrapper for the untenanted, the refusing and the tenant-scoped spawn
    /// doors, because a third copy of the reading below is how one of them ends
    /// up reporting a cancelled body as a completed one.
    fn place_body<F, Fut>(&mut self, lease: Lease, body: F) -> TaskId
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let token = self.child_token();
        let future = body(token.clone());
        self.place(lease, async move {
            future.await;
            // The reading is taken at the instant the body returned, so it is a
            // fact about the supervisor's own state rather than a guess at the
            // body's intent: a body that returned while cancellation was in
            // force is reported cancelled, and one that returned with no
            // cancellation in force is reported completed.
            if token.is_cancelled() {
                TaskEnd::Cancelled
            } else {
                TaskEnd::Completed
            }
        })
    }

    /// Take a permit if one is free, without counting anything.
    ///
    /// The non-counting half of [`Self::claim_now`], split out because
    /// [`Self::claim`]'s fast path must *not* count a full pool as a refusal: a
    /// supervisor at its bound is healthy and waiting, which is backpressure, and
    /// counting it as a refusal is the mistake this split exists to prevent.
    fn try_take(&mut self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.permits).try_acquire_owned().ok()
    }

    /// Give the future a [`TaskId`], place it, and register the identity.
    ///
    /// The identity is registered before the task can run to completion,
    /// because the spawn and the insert happen on this thread with no await
    /// between them, so a terminal report always finds its entry.
    ///
    /// The id is returned rather than dropped because a caller sometimes has to
    /// name the task it just started — [`Supervisor::spawn_process`] reports it
    /// so the process's terminal outcome can be matched to the spawn — and the
    /// id is not a handle: it cannot join, abort or observe the task.
    fn place<Fut>(&mut self, lease: Lease, future: Fut) -> TaskId
    where
        Fut: Future<Output = TaskEnd> + Send + 'static,
    {
        let task = self.allocate_task_id();
        self.place_owned(task, async move {
            let _lease = lease;
            future.await
        });
        task
    }

    /// Place a future whose native owner already holds the admission permit.
    fn place_owned<Fut>(&mut self, task: TaskId, future: Fut) -> TaskId
    where
        Fut: Future<Output = TaskEnd> + Send + 'static,
    {
        self.spawned = self.spawned.saturating_add(1);
        let handle = self.set.spawn(future);
        self.identities.insert(handle.id(), task);
        task
    }

    /// Allocate the stable id before constructing a resource-owning future.
    fn allocate_task_id(&mut self) -> TaskId {
        let task = TaskId(self.next_task);
        self.next_task = self.next_task.saturating_add(1);
        task
    }

    #[cfg(all(unix, feature = "process"))]
    /// Refresh retained owners and append terminal receipts when report capacity
    /// allows it.
    fn refresh_cleanup_owners(&mut self) {
        for (task, containment) in self.cleanup_owners.drive(&NATIVE_GROUP_OBSERVER, |task| {
            self.identities.values().any(|live| *live == task)
        }) {
            if self.reports.len() < self.report_cap {
                self.reports.push_back(TaskOutcome::CleanupSettled {
                    task,
                    cleanup: settled_receipt(&containment),
                    containment,
                });
            } else {
                self.reports_dropped = self.reports_dropped.saturating_add(1);
            }
        }
    }

}
