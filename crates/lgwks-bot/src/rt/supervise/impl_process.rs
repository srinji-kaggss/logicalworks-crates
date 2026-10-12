impl Supervisor {
    /// Start `command` as a supervised child process.
    ///
    /// This is the only way this crate starts a process, and it behaves exactly
    /// like [`Supervisor::spawn`] where it matters — it awaits a free slot
    /// before starting anything, it returns no handle, and the process it
    /// starts is reported like any other task:
    ///
    /// - **Bounded.** The permit is acquired before the spawn, so a supervisor
    ///   already at its ceiling does not start the process until a slot frees.
    ///   Nothing is queued on this module's behalf.
    /// - **Killed on drop, as a whole group.** The supervisor's token reaches
    ///   the task, and the task kills the child's **process group** rather than
    ///   the child alone: a shell started here can spawn a pipeline, and
    ///   `Child::kill` would leave the grandchildren running with nobody
    ///   holding their handles. The same kill lands if the task is aborted,
    ///   because the guard that performs it lives in the task's own frame.
    /// - **Reported.** A process that exited zero is [`TaskOutcome::Completed`];
    ///   one that exited non-zero, was killed by a signal, or whose status
    ///   could not be read is [`TaskOutcome::Failed`], and carries its
    ///   [`ExitStatus`] when there is one; one this supervisor stopped because
    ///   it was cancelled is [`TaskOutcome::Cancelled`]. [`Stats::failed`]
    ///   counts the middle case, so a bot running a failing command is not
    ///   reported as a healthy one.
    ///
    /// # The handle this does not return
    ///
    /// The [`TaskId`] is not a handle. It cannot be joined, aborted, or waited
    /// on; it exists so a caller can match a terminal report to the command it
    /// started, which is the only thing a caller needs a running process to
    /// give back. A caller that wants the child's output redirects that stream
    /// into a sink it already owns — the supervisor owns the process, not the
    /// data.
    ///
    /// # Why the description is borrowed
    ///
    /// The description is data, not a running handle. Borrowing it lets the
    /// supervisor build the private engine command while the caller retains the
    /// auditable specification:
    ///
    /// ```no_run
    /// # use lgwks_bot::rt::process::ProcessSpec;
    /// # use lgwks_bot::rt::supervise::Supervisor;
    /// # async fn run() -> std::io::Result<()> {
    /// # let mut supervisor = Supervisor::default();
    /// let mut spec = ProcessSpec::new("sh");
    /// spec.arg("-c").arg("exit 0");
    /// supervisor.spawn_process(&spec).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// The caller may retain and clone the description, but cannot start a child
    /// or obtain a child handle from it.
    ///
    /// # Errors
    ///
    /// An [`io::Error`] from the engine when the process could not be started
    /// at all: the program does not exist, is not executable, or the OS refused
    /// the fork. A failure to *start* is returned rather than turned into an
    /// outcome, because nothing was started and the caller is the one who knows
    /// what that means. The unreachable case — this module never closes its own
    /// semaphore — is returned too rather than asserted, since a
    /// `forbid`-level lint rules out the panic and a silent `return` would
    /// swallow the condition. A refused start does not consume a slot: the
    /// permit is released when this function returns.
    #[cfg(all(unix, feature = "process"))]
    pub async fn spawn_process(&mut self, spec: &ProcessSpec) -> io::Result<TaskId> {
        let (task, ()) = self.spawn_process_reading(spec, |_| Ok(())).await?;
        Ok(task)
    }

    /// Start `spec` exactly as [`Supervisor::spawn_process`] does, and also
    /// return the identity of the process that leads its group.
    ///
    /// The identity — the leader's pid and the instant the OS records it started
    /// — is what a *successor* needs when this supervisor is killed with SIGKILL
    /// and runs no cleanup: the child's group outlives it, and a later process
    /// that stored the identity hands it to
    /// [`reap_orphaned_group`](crate::rt::process::reap_orphaned_group), which
    /// stops the group only while the pid still names this leader. Store it
    /// durably (a database row) before relying on it; this method does not.
    ///
    /// The identity is read after the child starts and before the supervisor
    /// can reap it, so the pid cannot have been reissued yet. On Linux the read
    /// is two `/proc` files; elsewhere it runs `ps` once on the calling thread,
    /// which is why it is a separate door: [`Supervisor::spawn_process`] does
    /// not pay for it.
    ///
    /// # Errors
    ///
    /// Every error [`Supervisor::spawn_process`] returns, and the read's own
    /// error when the leader's identity could not be read. A spawn whose
    /// identity is unknown is refused rather than returned half-recorded: its
    /// group is killed by the same guard that kills an aborted task's group, so
    /// no unrecorded group is left running.
    #[cfg(all(unix, feature = "process"))]
    pub async fn spawn_process_identified(
        &mut self,
        spec: &ProcessSpec,
    ) -> io::Result<IdentifiedSpawn> {
        let (task, leader) = self.spawn_process_reading(spec, identify_leader).await?;
        Ok(IdentifiedSpawn { task, leader })
    }

    /// The one admission, start and placement both process spawns share.
    ///
    /// `read` sees the leader's pid while the child is started, guarded and not
    /// yet reaped, and what it returns travels back beside the task id. A
    /// refusal from it drops the guards built so far, which kill the group.
    #[cfg(all(unix, feature = "process"))]
    async fn spawn_process_reading<T>(
        &mut self,
        spec: &ProcessSpec,
        read: impl FnOnce(i32) -> io::Result<T>,
    ) -> io::Result<(TaskId, T)> {
        let permit = self.admit_process().await?;
        let (token, bounds) = self.prepare_process(spec);
        let clock = self.clock.clone();
        let (child, group_id) = start(spec)?;
        // Construct the guard before the child is placed in the async task. If
        // the task is aborted before its first poll, this guard still owns the
        // cleanup fallback for a process that native spawning already started.
        let task = self.allocate_task_id();
        #[cfg(all(target_os = "linux", feature = "process"))]
        let scope = scope_tree(group_id);
        #[cfg(all(target_os = "linux", feature = "process"))]
        let group = ProcessGroup::of_scoped(
            group_id,
            task,
            permit,
            Arc::clone(&self.cleanup_owners),
            scope,
        );
        #[cfg(all(unix, not(target_os = "linux"), feature = "process"))]
        let group = ProcessGroup::of(group_id, task, permit, Arc::clone(&self.cleanup_owners));
        let live = LiveProcess::enter(&self.cleanup_owners);
        let read = read(group_id)?;
        let placed = self.place_owned(task, async move {
            let end = drive_process(&clock, child, &token, group, bounds).await;
            drop(live);
            task_end(end)
        });
        Ok((placed, read))
    }

    /// The admission both process spawns share, on every target that has a
    /// process owner: a slot, and a cancellation recheck before anything forks.
    ///
    /// The fence inside `claim` answers for capacity and for cancellation alike,
    /// so this names which world refused, and a cancelled supervisor's caller
    /// does not read the refusal as a platform failure. The recheck at the owned
    /// admission point comes before the command is built: a cancelled supervisor
    /// never forks, so the first instruction of a refused command does not run.
    /// The permit drops with the refusal, and the refusal is counted.
    #[cfg(all(any(unix, windows), feature = "process"))]
    async fn admit_process(&mut self) -> io::Result<Lease> {
        let Some(permit) = self.claim().await else {
            if self.token.is_cancelled() {
                let refusal = Err(io::Error::other(SupervisorCancelled));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "spawn_process: returning an error to the caller");
                return refusal;
            }
            let refusal = Err(io::Error::other(
                "lgwks_bot: the supervisor's in-flight semaphore was closed",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "spawn_process: returning an error to the caller");
            return refusal;
        };
        if self.token.is_cancelled() {
            self.refused = self.refused.saturating_add(1);
            let refusal = Err(io::Error::other(SupervisorCancelled));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "spawn_process: returning an error to the caller");
            return refusal;
        }
        Ok(permit)
    }

    /// Run `spec` to completion and return its captured report.
    ///
    /// This is the result-bearing counterpart of [`Supervisor::spawn_process`],
    /// and it shares that method's ownership, deadline and cleanup machinery
    /// through the same private driver: the child is placed in its own process
    /// group, the deadline kills the **group** (INV-BOT-9), and the run reports
    /// the same [`CleanupReceipt`]. Where `spawn_process` hands back a
    /// [`TaskId`] and reports later, this awaits the child and returns what it
    /// observed.
    ///
    /// - **Bounded.** A slot is claimed before the fork, so a supervisor at its
    ///   ceiling waits for one rather than starting anyway. The permit is held
    ///   for the whole run and released when it ends.
    /// - **Captured.** A stream with [`StdioPolicy::Capture`](crate::rt::process::StdioPolicy::Capture)
    ///   is retained up to its ceiling and drained past it, and
    ///   [`ProcessRun`] reports the retained bytes, the exact total and whether
    ///   the stream was truncated.
    /// - **Truthful about a stop.** A deadline kill is reported through
    ///   [`ProcessRun::deadline_fired`], not as a normal exit, and its cleanup
    ///   receipt is the group's.
    /// - **Observable while it runs.** `on_line`, when given, is called once per
    ///   newline-terminated line of **stdout** as the child writes it, from the
    ///   same pipe read that retains the output. It is what makes a service's
    ///   readiness an observation of the child's output rather than a timer: the
    ///   caller's own readiness gate is closed on the line the child printed,
    ///   while the child is still running, and a child that never prints is
    ///   never released. The observer is called only for lines the capture read,
    ///   so a spec whose stdout is not [`StdioPolicy::Capture`](crate::rt::process::StdioPolicy::Capture)
    ///   observes nothing — the caller declares the capture, and this is the same
    ///   declaration. See [`Supervisor::run_process_observed`].
    ///
    /// # Errors
    ///
    /// [`ProcessRunError::Refused`] when the supervisor was cancelled before
    /// the fork, and [`ProcessRunError::NotStarted`] when the platform refused
    /// the program — both establish that nothing ran. A failure *after* the
    /// child started is [`ProcessRunError::AfterStart`], which establishes the
    /// opposite and is why the two are distinguishable.
    ///
    /// One method on every target: the Unix body owns the child's process group,
    /// the Windows body owns its tree through a job object (and refuses a spec
    /// that captures a stream, which that path cannot read), and any other target
    /// answers with a typed pre-fork refusal. Choosing between them behind one
    /// signature is what keeps a caller from having to know which platform it
    /// compiled for before it can run a command.
    #[cfg(feature = "process")]
    pub async fn run_process(&mut self, spec: &ProcessSpec) -> Result<ProcessRun, ProcessRunError> {
        self.run_process_observed(spec, None).await
    }

    /// [`Supervisor::run_process`] with an observer on the child's stdout lines.
    ///
    /// The form a service's readiness takes. The observer is a `&dyn Fn` rather
    /// than a boxed closure because it lives only for the run's duration and
    /// never crosses a thread: it is called on the task driving this child, from
    /// inside the pipe read, between the child's write and this caller's next
    /// observation. A `None` observer is the same call with no observation, and
    /// is what [`Supervisor::run_process`] does.
    ///
    /// # Errors
    ///
    /// As [`Supervisor::run_process`]: the observer changes what the caller
    /// observes *while* the child runs, never what the run reports after it.
    #[cfg(all(any(unix, windows), feature = "process"))]
    pub async fn run_process_observed(
        &mut self,
        spec: &ProcessSpec,
        on_line: Option<LineObserver<'_>>,
    ) -> Result<ProcessRun, ProcessRunError> {
        let Some(lease) = self.claim().await else {
            let refusal = Err(ProcessRunError::Refused);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "run_process_observed: returning an error to the caller");
            return refusal;
        };
        self.drive_run(lease, spec, on_line).await
    }

    /// Run `spec` to completion under `tenant`, charging the admission to that
    /// tenant, and return its captured report.
    ///
    /// The tenant-scoped counterpart of [`Supervisor::run_process`]: the same
    /// one process driver, the same ownership, deadline and cleanup, with the
    /// admission charged to `tenant` and a typed refusal when that tenant's
    /// bounded queue is full. The supervisor's global ceiling still bounds the
    /// total in-flight processes; `tenant`'s own share of them is what this pins.
    ///
    /// # Errors
    ///
    /// [`RunForError::Refused`] carrying [`SpawnRefused::TenantAtCapacity`] when
    /// `tenant`'s queue is at its bound, or [`SpawnRefused::Cancelled`] when the
    /// supervisor has stopped admitting — in both cases nothing ran. Otherwise
    /// [`RunForError::Run`] carrying exactly what [`Supervisor::run_process`]
    /// returns: [`ProcessRunError::NotStarted`] also establishes that nothing
    /// ran, and [`ProcessRunError::AfterStart`] the opposite.
    ///
    /// Two arms rather than one because "this tenant is at its bound" and "the
    /// command could not start" are different facts for the same caller: the
    /// first is the caller's own load to fix, the second is the platform's.
    #[cfg(all(any(unix, windows), feature = "process", feature = "script"))]
    pub async fn run_process_for(
        &mut self,
        tenant: &Tenant,
        spec: &ProcessSpec,
    ) -> Result<ProcessRun, RunForError> {
        let Some(shell) = self.tenancy.clone() else {
            // No policy: this run is the implicit tenant's, admitted exactly as
            // `run_process` admits it.
            return self.run_process(spec).await.map_err(RunForError::Run);
        };
        self.reap();
        let lease = match self.claim_tenanted(&shell, tenant).await {
            Ok(lease) => lease,
            Err(refusal) => {
                self.refused = self.refused.saturating_add(1);
                let refused = Err(RunForError::Refused(refusal));
                lgwks_std::trace::debug!(error = ?refused.as_ref().err(), "run_process_for: returning an error to the caller");
                return refused;
            }
        };
        self.drive_run(lease, spec, None)
            .await
            .map_err(RunForError::Run)
    }

    /// The process driver every runner shares, from an admitted lease to a
    /// report.
    ///
    /// Split out of [`Supervisor::run_process_observed`] so the untenanted and
    /// the tenant-scoped runners are one body rather than two: a caller that
    /// named a tenant and a caller that did not fork the same child, cleaned up
    /// the same process group and reported the same receipt. The lease carries
    /// the permit — and, under a policy, the tenant it is charged to — through
    /// the whole run, so capacity comes back through the same door whether the
    /// call named a tenant or not.
    #[cfg(all(unix, feature = "process"))]
    async fn drive_run(
        &mut self,
        lease: Lease,
        spec: &ProcessSpec,
        on_line: Option<LineObserver<'_>>,
    ) -> Result<ProcessRun, ProcessRunError> {
        // The same owned admission point `spawn_process` rechecks: a cancelled
        // supervisor never forks, so the first instruction does not run.
        if self.token.is_cancelled() {
            self.refused = self.refused.saturating_add(1);
            let refusal = Err(ProcessRunError::Refused);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "drive_run: returning an error to the caller");
            return refusal;
        }
        let (token, bounds) = self.prepare_process(spec);
        let clock = self.clock.clone();
        let (child, group_id) = match start(spec) {
            Ok(started) => started,
            Err(source) => {
                let refusal = Err(ProcessRunError::NotStarted { source });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "drive_run: returning an error to the caller");
                return refusal;
            }
        };
        let task = self.allocate_task_id();
        #[cfg(all(target_os = "linux", feature = "process"))]
        let scope = scope_tree(group_id);
        #[cfg(all(target_os = "linux", feature = "process"))]
        let group = ProcessGroup::of_scoped(
            group_id,
            task,
            lease,
            Arc::clone(&self.cleanup_owners),
            scope,
        );
        #[cfg(all(unix, not(target_os = "linux"), feature = "process"))]
        let group = ProcessGroup::of(group_id, task, lease, Arc::clone(&self.cleanup_owners));
        self.spawned = self.spawned.saturating_add(1);
        let end = drive_process_observed(&clock, child, &token, group, bounds, on_line).await;
        self.finish_run(end)
    }

    /// Count a finished run and build its report, on every target that has a
    /// process owner.
    ///
    /// The tail both `drive_run` bodies share, so a run on a process group and a
    /// run in a job object count, classify and report an ending the same way.
    #[cfg(all(any(unix, windows), feature = "process"))]
    fn finish_run(&mut self, end: ProcessEnd) -> Result<ProcessRun, ProcessRunError> {
        self.completed = self.completed.saturating_add(1);
        let deadline_fired = matches!(end.observation, ProcessObservation::Deadline);
        let settled = match end.observation {
            ProcessObservation::Exited => {
                if end.status.is_some_and(|status| status.success()) {
                    self.succeeded = self.succeeded.saturating_add(1);
                } else {
                    self.failed = self.failed.saturating_add(1);
                }
                true
            }
            ProcessObservation::Deadline | ProcessObservation::Unobservable => {
                self.failed = self.failed.saturating_add(1);
                true
            }
            // Unreachable in practice: the child token is a child of a
            // supervisor this call borrows exclusively, so nothing can cancel
            // it while the run is awaited. Reported rather than asserted, and
            // as permanently indeterminate rather than as a normal result.
            ProcessObservation::Cancelled => {
                self.cancelled = self.cancelled.saturating_add(1);
                false
            }
        };
        if !settled {
            let refusal = Err(ProcessRunError::AfterStart {
                source: io::Error::new(
                    io::ErrorKind::Interrupted,
                    "the supervisor was cancelled while the process ran",
                ),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "drive_run: returning an error to the caller");
            return refusal;
        }
        // A status is the command's *own* exit. A deadline kill reaps the group
        // and the engine then reports the signal, but that is the supervisor's
        // stop rather than the command's outcome, so it is not carried as a
        // status — the same reading `spawn_process` reports.
        let status = if matches!(end.observation, ProcessObservation::Exited) {
            end.status
        } else {
            None
        };
        Ok(ProcessRun {
            status,
            deadline_fired,
            stdout: end.stdout,
            stderr: end.stderr,
            cleanup: end.cleanup.receipt,
            containment: end.cleanup.containment,
        })
    }

    /// Process containment is unavailable beyond Unix and Windows targets.
    #[cfg(all(not(any(unix, windows)), feature = "process"))]
    pub async fn run_process_observed(
        &mut self,
        spec: &ProcessSpec,
        on_line: Option<LineObserver<'_>>,
    ) -> Result<ProcessRun, ProcessRunError> {
        let _ = (spec, on_line);
        Err(ProcessRunError::NotStarted {
            source: Self::no_process_group(),
        })
    }

    /// Process containment is unavailable beyond Unix and Windows targets, so no
    /// tenant's run ever starts and the admission is never charged.
    #[cfg(all(not(any(unix, windows)), feature = "process", feature = "script"))]
    pub async fn run_process_for(
        &mut self,
        tenant: &Tenant,
        spec: &ProcessSpec,
    ) -> Result<ProcessRun, RunForError> {
        let _ = (tenant, spec);
        let refusal = Err(RunForError::Run(ProcessRunError::NotStarted {
            source: Self::no_process_group(),
        }));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "run_process_for: returning an error to the caller");
        refusal
    }

    /// Process containment is unavailable beyond Unix and Windows targets.
    #[cfg(all(not(any(unix, windows)), feature = "process"))]
    pub async fn spawn_process(&mut self, spec: &ProcessSpec) -> io::Result<TaskId> {
        let _ = spec;
        Err(Self::no_process_group())
    }

    /// Process containment is unavailable beyond Unix and Windows targets.
    #[cfg(all(not(any(unix, windows)), feature = "process"))]
    pub async fn spawn_process_identified(
        &mut self,
        spec: &ProcessSpec,
    ) -> io::Result<IdentifiedSpawn> {
        let _ = spec;
        Err(Self::no_process_group())
    }

    /// The one refusal every process door returns on a target with neither
    /// process groups nor job objects.
    ///
    /// Both doors say the same thing for the same reason — there is no process
    /// owner to own, reap or clean up on this target — so the message is written
    /// once. A second copy is a second statement about the same boundary, and
    /// the two would drift.
    #[cfg(all(not(any(unix, windows)), feature = "process"))]
    fn no_process_group() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "lgwks_bot: supervised process cleanup needs process groups or job objects",
        )
    }
}
