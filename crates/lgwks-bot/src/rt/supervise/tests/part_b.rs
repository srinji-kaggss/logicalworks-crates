    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn pending_cleanup_owner_keeps_its_permit_until_later_absence_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::CleanupOwners;
        use crate::rt::sync::Semaphore;

        let semaphore = Arc::new(Semaphore::new(1));
        let Some(permit) = Arc::clone(&semaphore).try_acquire_owned().ok() else {
            return Err("the test semaphore started without its one permit".into());
        };
        let owners = Arc::new(CleanupOwners::default());
        let task = TaskId(9);
        let cleanup_observer = sequence(usize::MAX);
        let signaller = recording();
        let mut group = armed_group(
            42,
            task,
            false,
            Some(Lease::plain(permit)),
            Arc::clone(&owners),
            group_seams(&signaller, &cleanup_observer),
        );

        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupPending,
            "exhausting bounded signal attempts remains pending"
        );
        group.mark_reaped();
        assert_eq!(
            block_on(group.confirm_absence()),
            CleanupReceipt::CleanupPending,
            "a bounded post-reap pass cannot claim absence"
        );
        let later_observer = sequence(1);
        drop(group);
        assert_eq!(
            semaphore.available_permits(),
            0,
            "dropping a pending task-local guard must transfer, not release, its lease"
        );
        assert_eq!(owners.pending_count(), 1, "cleanup remains owned");
        assert!(
            owners.drive(&FailingGroupObserver, |_| false).is_empty(),
            "an observation error cannot release cleanup ownership"
        );
        assert_eq!(semaphore.available_permits(), 0);
        assert!(
            owners.drive(&later_observer, |_| false).is_empty(),
            "a still-present group cannot produce a terminal receipt"
        );
        assert_eq!(semaphore.available_permits(), 0);
        assert!(
            owners
                .drive(&later_observer, |candidate| candidate == task)
                .is_empty(),
            "absence before the task report must retain attribution and capacity"
        );
        assert_eq!(semaphore.available_permits(), 0);
        let settled = owners.drive(&later_observer, |_| false);
        // Attribution and capacity are this test's subject; what the report says
        // about the tree is the containment family's subject.
        assert_eq!(
            settled_tasks(&settled),
            vec![task],
            "later absence is attributed to its task"
        );
        assert_eq!(
            semaphore.available_permits(),
            1,
            "terminal proof releases capacity"
        );
        assert_eq!(owners.pending_count(), 0);
        assert_eq!(
            signaller.calls.load(Ordering::Relaxed),
            1,
            "one signal from the drain, and none after the reap: the id is released then, and \
             the drop-time fallback must not signal a number the OS may reissue"
        );
        Ok(())
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn cleanup_failed_transfers_its_lease_until_absence_is_observed()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::CleanupOwners;
        use crate::rt::sync::Semaphore;

        let semaphore = Arc::new(Semaphore::new(1));
        let Some(permit) = Arc::clone(&semaphore).try_acquire_owned().ok() else {
            return Err("the test semaphore started without its one permit".into());
        };
        let owners = Arc::new(CleanupOwners::default());
        let task = TaskId(10);
        let observer = sequence(0);
        let signaller = recording();
        let mut group = armed_group(
            43,
            task,
            true,
            Some(Lease::plain(permit)),
            Arc::clone(&owners),
            group_seams(&signaller, &observer),
        );

        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupFailed,
            "a guard whose leader was reaped cannot signal a possibly reused id"
        );
        drop(group);
        assert_eq!(owners.pending_count(), 1, "failed cleanup remains owned");
        assert_eq!(semaphore.available_permits(), 0);
        assert_eq!(
            settled_tasks(&owners.drive(&observer, |_| false)),
            vec![task],
            "an absent group settles the obligation it was transferred for"
        );
        assert_eq!(semaphore.available_permits(), 1);
        assert_eq!(signaller.calls.load(Ordering::Relaxed), 0);
        Ok(())
    }

    /// An adoptee another waiter reaped first is settled on the first pass,
    /// not waited on for the whole death bound.
    ///
    /// The other waiter is the test's own `wait`, standing in for a second
    /// drain's sweep: after it, `waitpid` on the pid answers `ECHILD`, which is
    /// what held a drain for 400 passes and had shutdown abort it.
    #[cfg(all(target_os = "linux", feature = "process"))]
    #[test]
    fn an_adoptee_another_waiter_reaped_is_settled_on_the_first_pass()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut child = std::process::Command::new("sleep").arg("30").spawn()?;
        let pid = i32::try_from(child.id())?;
        child.kill()?;
        let _status = child.wait()?;

        let settled = block_on(super::reap_adopted(vec![pid]));

        assert!(
            settled.unreaped.is_empty(),
            "a pid this process no longer holds is not owed a reap: {:?}",
            settled.unreaped
        );
        assert_eq!(
            settled.passes, 1,
            "it is settled by the pass that finds it gone, not by the bound"
        );
        Ok(())
    }

    /// The control: an adoptee still this process's child is reaped here, so
    /// settling a pid nobody holds did not stop the reap of one somebody does.
    #[cfg(all(target_os = "linux", feature = "process"))]
    #[test]
    fn a_killed_adoptee_still_held_is_reaped_by_the_drain() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut child = std::process::Command::new("sleep").arg("30").spawn()?;
        let pid = i32::try_from(child.id())?;
        child.kill()?;

        let settled = block_on(super::reap_adopted(vec![pid]));

        assert!(
            settled.unreaped.is_empty(),
            "a killed child dies inside the bound and is reaped: {:?}",
            settled.unreaped
        );
        assert!(
            lgwks_std::process::adopted_descendants(&[pid])?.is_empty(),
            "the reap collected the zombie, so the pid is no longer this process's child"
        );
        Ok(())
    }

    /// A quiet window outlasts the longest wait one drain makes on its own
    /// bounds, so a drain still inside a bound it was given is never aborted as
    /// stalled (INV-BOT-156).
    #[cfg(all(target_os = "linux", feature = "process"))]
    #[test]
    fn the_cleanup_grace_outlasts_a_drains_own_longest_wait() {
        let reap = super::ADOPTION_REAP_INTERVAL.saturating_mul(super::ADOPTION_REAP_PASSES);
        let sweep = super::ADOPTION_REAP_INTERVAL.saturating_mul(super::SWEEP_CONFIRM_PASSES);
        assert!(
            super::PROCESS_CLEANUP_GRACE > reap.saturating_add(sweep),
            "grace {:?} must outlast the adoption reap {reap:?} plus the sweep {sweep:?}",
            super::PROCESS_CLEANUP_GRACE
        );
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn shutdown_report_keeps_pending_cleanup_and_emits_later_terminal_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::rt::sync::Semaphore;

        let semaphore = Arc::new(Semaphore::new(1));
        let Some(permit) = Arc::clone(&semaphore).try_acquire_owned().ok() else {
            return Err("the test semaphore started without its one permit".into());
        };
        let task = TaskId(11);
        let report = block_on(Supervisor::new(1).shutdown());
        report
            .cleanup_owners
            .register(task, 44, Lease::plain(permit), whole_capture());
        assert_eq!(report.pending_cleanup_count(), 1);
        assert!(
            !report.is_clean(),
            "an unresolved native resource is not clean"
        );
        let Err(mut report) = report.into_outcomes() else {
            return Err("into_outcomes consumed a report with pending cleanup".into());
        };
        assert_eq!(
            report.pending_cleanup_count(),
            1,
            "Err must retain ownership"
        );
        assert_eq!(semaphore.available_permits(), 0);

        assert_eq!(
            report.reap_pending_cleanups_with(&FailingGroupObserver),
            0,
            "an observer error is not a terminal receipt"
        );
        let observer = sequence(1);
        assert_eq!(report.reap_pending_cleanups_with(&observer), 0);
        assert_eq!(report.pending_cleanup_count(), 1);
        assert_eq!(report.reap_pending_cleanups_with(&observer), 1);
        assert_eq!(report.pending_cleanup_count(), 0);
        assert_eq!(semaphore.available_permits(), 1);
        assert!(report.is_clean());
        let outcomes = match report.into_outcomes() {
            Ok(outcomes) => outcomes,
            Err(_) => return Err("settled cleanup report remained unavailable".into()),
        };
        assert_eq!(
            outcomes,
            vec![TaskOutcome::CleanupSettled {
                task,
                cleanup: CleanupReceipt::CleanupConfirmed,
                containment: whole_capture(),
            }]
        );
        Ok(())
    }

    /// Drive the current task until nothing the supervisor started is still
    /// running.
    ///
    /// Returns `false` if the spin cap was reached, which would mean a task
    /// never completed. A test that asserts on the return value turns a hang
    /// into a failure instead of a timeout.
    ///
    /// It must be awaited on the runtime that spawned the tasks. [`block_on`]
    /// builds a current-thread runtime per call, so a task spawned in one call
    /// is aborted when that call's runtime is dropped and cannot be observed
    /// from the next, which is why every test here wraps its whole body in a
    /// single `block_on` rather than reaching for one per step.
    async fn settle(supervisor: &mut Supervisor) -> bool {
        let mut spins: u32 = 0;
        loop {
            supervisor.reap();
            if supervisor.stats().in_flight() == 0 {
                return true;
            }
            if spins >= 100_000 {
                return false;
            }
            spins = spins.saturating_add(1);
            yield_now().await;
        }
    }

    /// Drain every terminal outcome the supervisor is holding, oldest first.
    fn drain(supervisor: &mut Supervisor) -> Vec<TaskOutcome> {
        let mut outcomes = Vec::new();
        while let Some(outcome) = supervisor.next_report() {
            outcomes.push(outcome);
        }
        outcomes
    }

    /// The identities of every outcome satisfying `keep`, in completion order.
    ///
    /// Attribution is asserted through this rather than by matching on the
    /// outcome variants: completion order between two tasks that finish in the
    /// same scheduling pass is the engine's business, not this module's
    /// contract, so a test that pinned it would be pinning the wrong thing.
    fn tasks_where(outcomes: &[TaskOutcome], keep: impl Fn(&TaskOutcome) -> bool) -> Vec<TaskId> {
        outcomes
            .iter()
            .filter(|outcome| keep(outcome))
            .map(TaskOutcome::task)
            .collect()
    }

    /// Yield to the executor until `predicate` holds, or `limit` yields have
    /// happened.
    ///
    /// Returns whether the predicate held. A bounded spin rather than a timer:
    /// the executor under test here is the one a timer would need, so waiting
    /// for the thing being measured is the failure mode this helper exists to
    /// avoid.
    async fn yield_until(mut predicate: impl FnMut() -> bool, limit: u32) -> bool {
        let mut yields: u32 = 0;
        while !predicate() {
            if yields >= limit {
                return false;
            }
            yields = yields.saturating_add(1);
            yield_now().await;
        }
        true
    }

    /// Start a task that counts heartbeat increments until it is cancelled.
    ///
    /// It only advances when the executor schedules it, which is what makes it
    /// an observation of executor sharing rather than of wall-clock time.
    async fn spawn_heartbeat(supervisor: &mut Supervisor, beats: &Arc<AtomicU64>) {
        let beats = Arc::clone(beats);
        supervisor
            .spawn(move |token| async move {
                while !token.is_cancelled() {
                    beats.fetch_add(1, Ordering::SeqCst);
                    yield_now().await;
                }
            })
            .await;
    }

    #[test]
    fn a_large_finite_budget_still_yields_to_a_sibling_task() {
        // A body that resolves immediately never suspends the loop, so before
        // the loop yielded, one poll of it ran the entire budget and no other
        // task on the executor got to run at all. The budget is finite so the
        // old behaviour is a failed assertion here rather than a hung suite; a
        // current-thread runtime was the case that deadlocked outright, because
        // the task that would have cancelled could not even be scheduled.
        const BUDGET: u64 = 1_000_000;
        block_on(async {
            let beats = Arc::new(AtomicU64::new(0));
            let ran = Arc::new(AtomicU64::new(0));
            // The heartbeat count the body saw on its *first* iteration, and the
            // highest it saw on any later one. The heartbeat advancing between
            // two of the loop's own iterations is the observation that the
            // executor was shared: one uninterruptible poll of the loop sees a
            // single frozen value for its whole run, whatever that value is.
            let first_seen = Arc::new(AtomicU64::new(u64::MAX));
            let later_seen = Arc::new(AtomicU64::new(0));

            let mut supervisor = Supervisor::new(4);
            spawn_heartbeat(&mut supervisor, &beats).await;

            let body_ran = Arc::clone(&ran);
            let body_first = Arc::clone(&first_seen);
            let body_later = Arc::clone(&later_seen);
            let body_beats = Arc::clone(&beats);
            supervisor
                .spawn_repeating(budget_of(BUDGET), move |_tick| {
                    let body_ran = Arc::clone(&body_ran);
                    let body_first = Arc::clone(&body_first);
                    let body_later = Arc::clone(&body_later);
                    let body_beats = Arc::clone(&body_beats);
                    async move {
                        body_ran.fetch_add(1, Ordering::SeqCst);
                        let now = body_beats.load(Ordering::SeqCst);
                        if now < body_first.load(Ordering::SeqCst) {
                            body_first.store(now, Ordering::SeqCst);
                        }
                        if now > body_later.load(Ordering::SeqCst) {
                            body_later.store(now, Ordering::SeqCst);
                        }
                    }
                })
                .await;

            // Give the repeating worker its first poll before asserting on what
            // it observed: a worker cancelled before it starts proves nothing.
            assert!(
                yield_until(|| ran.load(Ordering::SeqCst) > 0, 1_000).await,
                "the repeating worker never took a single iteration"
            );
            assert!(
                yield_until(
                    || later_seen.load(Ordering::SeqCst) > first_seen.load(Ordering::SeqCst),
                    1_000
                )
                .await,
                "the repeating body ran {} of its {BUDGET} iterations seeing the heartbeat count \
                 frozen at {}, so one poll of it held the executor for the whole run",
                ran.load(Ordering::SeqCst),
                first_seen.load(Ordering::SeqCst)
            );

            supervisor.shutdown().await;
        });
    }

    #[test]
    fn a_cancel_from_a_sibling_task_stops_an_ongoing_ready_body() {
        // The case from the issue, in miniature: a loop whose body resolves
        // immediately, so it never suspends of its own accord, and a canceller
        // on a *different* task. The canceller can only run if the loop hands
        // the executor back, so one uninterruptible poll of the loop makes the
        // cancel unobservable — which on a current-thread runtime is not a
        // delay, it is a deadlock, because the canceller may never run at all.
        //
        // The body carries a brake so an unbounded loop still ends when the
        // cancel cannot land, turning the old behaviour into a failed assertion
        // rather than a hung suite. The assertions then distinguish the two:
        // the brake is three orders of magnitude above what a yielding loop
        // needs.
        const BRAKE: u64 = 1_000_000;
        block_on(async {
            let root = CancellationToken::new();
            let ran = Arc::new(AtomicU64::new(0));

            let mut supervisor = Supervisor::new(2);
            // The canceller holds the root, so its `cancel` reaches the loop's
            // child token. It waits for the loop's first iteration before
            // cancelling, so the cancel provably lands on a running loop rather
            // than on a task that never started.
            let canceller_root = root.clone();
            let canceller_ran = Arc::clone(&ran);
            supervisor
                .spawn(move |_task_token| async move {
                    while canceller_ran.load(Ordering::SeqCst) == 0 {
                        yield_now().await;
                    }
                    canceller_root.cancel();
                })
                .await;

            let loop_ran = Arc::clone(&ran);
            let loop_root = root.clone();
            let brake = root.clone();
            let outcome = repeat(&loop_root, Budget::Ongoing, move |_tick| {
                let brake = brake.clone();
                let loop_ran = Arc::clone(&loop_ran);
                async move {
                    let count = loop_ran.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                    if count >= BRAKE {
                        brake.cancel();
                    }
                }
            })
            .await;

            assert!(
                outcome.was_cancelled(),
                "the loop ended as {outcome:?} rather than as cancelled"
            );
            let iterations = outcome.iterations();
            assert!(
                iterations < BRAKE,
                "the loop ran to its own {BRAKE}-iteration brake ({iterations} iterations) \
                 instead of observing the cancel from the sibling task"
            );

            // Drain the canceller, then check the body is not invoked again:
            // the cancel is a boundary, not a hint.
            assert!(
                settle(&mut supervisor).await,
                "the canceller task never finished"
            );
            let settled = ran.load(Ordering::SeqCst);
            for _ in 0..64 {
                yield_now().await;
            }
            assert_eq!(
                ran.load(Ordering::SeqCst),
                settled,
                "the body was invoked again after the loop observed the cancel"
            );
        });
    }

    #[test]
    fn shutdown_lands_on_an_ongoing_ready_body() {
        // The shutdown half of the same defect: `Supervisor::shutdown` cancels
        // and drains, and before the loop yielded it could not even be reached
        // until the repeating task returned on its own.
        //
        // The brake holds the loop's *own* token — the one `repeat` races — so
        // an unbounded loop whose cancel cannot land ends at the brake rather
        // than running forever, which is what keeps this a failed assertion
        // instead of a hung suite.
        const BRAKE: u64 = 1_000_000;
        block_on(async {
            let ran = Arc::new(AtomicU64::new(0));
            let mut supervisor = Supervisor::new(1);
            let body_ran = Arc::clone(&ran);
            supervisor
                .spawn(move |token| async move {
                    let brake = token.clone();
                    let _outcome = repeat(&token, Budget::Ongoing, move |_tick| {
                        let brake = brake.clone();
                        let body_ran = Arc::clone(&body_ran);
                        async move {
                            let count = body_ran.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                            if count >= BRAKE {
                                brake.cancel();
                            }
                        }
                    })
                    .await;
                })
                .await;

            // Prove the loop is running before shutting down, so the shutdown
            // lands on a live body rather than on a task that never polled.
            assert!(
                yield_until(|| ran.load(Ordering::SeqCst) > 0, 1_000).await,
                "the repeating worker never took a single iteration"
            );
            supervisor.shutdown().await;

            let iterations = ran.load(Ordering::SeqCst);
            assert!(
                iterations < BRAKE,
                "the loop ran to its own {BRAKE}-iteration brake ({iterations} iterations) \
                 instead of being stopped by the shutdown"
            );
        });
    }

    #[test]
    fn an_already_cancelled_token_never_invokes_the_body() {
        let token = CancellationToken::new();
        token.cancel();
        let ran = AtomicU64::new(0);
        let body_ran = &ran;
        let outcome = block_on(repeat(&token, Budget::Ongoing, move |_tick| async move {
            body_ran.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(
            outcome,
            Outcome::Cancelled { iterations: 0 },
            "a token cancelled before the loop starts must stop it with no iterations"
        );
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "the body must not be invoked at all past a cancellation boundary"
        );
    }

    #[test]
    fn a_budget_of_iterations_stops_at_the_limit() {
        let token = CancellationToken::new();
        let ticks = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&ticks);
        let outcome = block_on(repeat(&token, budget_of(3), move |_tick| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }));
        assert_eq!(
            outcome,
            Outcome::Exhausted { iterations: 3 },
            "a three-iteration budget must run the body exactly three times"
        );
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            3,
            "the body must have been entered once per iteration"
        );
    }

    /// Run an ongoing `repeat` whose body cancels its own token on entry and
    /// then runs `rest`.
    fn repeat_cancelling_itself<Rest, Fut>(rest: Rest) -> Outcome
    where
        Rest: Fn() -> Fut,
        Fut: Future<Output = ()>,
    {
        let token = CancellationToken::new();
        let trigger = token.clone();
        block_on(repeat(&token, Budget::Ongoing, move |_tick| {
            trigger.cancel();
            rest()
        }))
    }

    #[test]
    fn an_ongoing_budget_stops_when_the_token_is_cancelled() {
        let outcome = repeat_cancelling_itself(|| async {});
        assert_eq!(
            outcome,
            Outcome::Cancelled { iterations: 1 },
            "a cancel during the first iteration must end the loop after it"
        );
    }

    #[test]
    fn cancellation_interrupts_a_body_that_is_still_awaiting() {
        // The body never resolves. If cancellation were only observed between
        // iterations, the loop would have to await it to completion and the
        // test would hang rather than fail.
        let outcome = repeat_cancelling_itself(pending::<()>);
        assert!(
            outcome.was_cancelled(),
            "an in-flight body must be dropped at the cancel, got {outcome:?}"
        );
    }

    #[test]
    fn a_budget_of_duration_expires() {
        let token = CancellationToken::new();
        let outcome = block_on(repeat(
            &token,
            Budget::For(Duration::ZERO),
            move |_tick| async {},
        ));
        assert_eq!(
            outcome,
            Outcome::Exhausted { iterations: 0 },
            "a zero duration must expire before the first iteration"
        );
    }

    #[test]
    fn a_supervisor_reports_what_it_has_run() {
        block_on(async {
            let mut supervisor = Supervisor::new(2);
            supervisor.spawn(|_token| async {}).await;
            supervisor.spawn(|_token| async {}).await;
            assert!(settle(&mut supervisor).await, "both tasks must finish");
            let stats = supervisor.stats();
            assert_eq!(stats.spawned, 2, "both tasks must be counted as spawned");
            assert_eq!(
                stats.completed, 2,
                "both tasks must be counted as completed"
            );
            assert_eq!(
                stats.succeeded, 2,
                "both tasks returned, so both must be counted as succeeded"
            );
            assert_eq!(stats.in_flight(), 0, "nothing must remain in flight");
            assert_eq!(stats.refused, 0, "nothing must have been refused");
        });
    }

    #[test]
    fn a_cancelled_supervisor_refuses_admission_without_constructing_the_body() {
        block_on(async {
            // Cancellation is the terminal admission state: after it, no body
            // constructor runs and no slot is taken, on either spawn path. The
            // constructor is the observable, not the body: a body that writes a
            // marker or takes a lock would prove the old behavior, where the
            // constructor ran before anything looked at the token.
            let mut supervisor = Supervisor::new(2);
            supervisor.cancel();

            let built = Arc::new(AtomicU64::new(0));
            supervisor
                .spawn({
                    let built = Arc::clone(&built);
                    move |_token| {
                        built.fetch_add(1, Ordering::Relaxed);
                        async {}
                    }
                })
                .await;
            let stats = supervisor.stats();
            assert_eq!(
                built.load(Ordering::Relaxed),
                0,
                "a cancelled supervisor must never construct the body it refuses"
            );
            assert_eq!(stats.spawned, 0, "a refused spawn must not be placed");
            assert_eq!(
                stats.refused, 1,
                "the waiting-spawn refusal must be counted"
            );

            let built_now = Arc::new(AtomicU64::new(0));
            let refused = supervisor.try_spawn({
                let built_now = Arc::clone(&built_now);
                move |_token| {
                    built_now.fetch_add(1, Ordering::Relaxed);
                    async {}
                }
            });
            assert_eq!(
                refused,
                Err(TrySpawnRefusal::Cancelled),
                "the immediate path must name cancellation, not capacity"
            );
            assert_eq!(
                built_now.load(Ordering::Relaxed),
                0,
                "the immediate path must also refuse before the constructor"
            );
            assert_eq!(supervisor.stats().refused, 2, "both refusals counted");
        });
    }

    /// Fill `supervisor`'s only slot with a body that waits for the returned
    /// gate and then sets the returned done flag.
    async fn occupy(supervisor: &mut Supervisor) -> (Arc<AtomicBool>, Arc<AtomicBool>) {
        let gate = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        supervisor
            .spawn({
                let gate = Arc::clone(&gate);
                let done = Arc::clone(&done);
                move |_token| async move {
                    while !gate.load(Ordering::Relaxed) {
                        yield_now().await;
                    }
                    done.store(true, Ordering::Relaxed);
                }
            })
            .await;
        (gate, done)
    }

    #[test]
    fn cancellation_closes_admission_even_after_a_slot_frees() {
        block_on(async {
            // The order is the point: cancel first, then let the only slot
            // free. A supervisor that checks capacity but not cancellation
            // admits at the freeing of the slot — the constructor below runs
            // and the counter moves. The fence must refuse at the owned
            // admission point regardless of what the pool is doing.
            let mut supervisor = Supervisor::new(1);
            let (gate, done) = occupy(&mut supervisor).await;
            supervisor.cancel();
            gate.store(true, Ordering::Relaxed);
            // Let the occupier finish and its permit return to the pool: the
            // body's own flag says it ran to its end, and the yields give the
            // wrapper around it — which holds the permit — the polls it needs
            // to complete behind it. The supervisor's counters are not the
            // signal here: a cancelled supervisor stops reaping, but the
            // permit lives in the task wrapper, not in the counters.
            for _ in 0..256 {
                if done.load(Ordering::Relaxed) {
                    break;
                }
                yield_now().await;
            }
            assert!(
                done.load(Ordering::Relaxed),
                "the occupier must have ended before the admission attempt"
            );

            let built = Arc::new(AtomicU64::new(0));
            let refused = supervisor.try_spawn({
                let built = Arc::clone(&built);
                move |_token| {
                    built.fetch_add(1, Ordering::Relaxed);
                    async {}
                }
            });
            assert_eq!(
                refused,
                Err(TrySpawnRefusal::Cancelled),
                "a freed slot is not an admission offer after cancellation"
            );
            assert_eq!(
                built.load(Ordering::Relaxed),
                0,
                "no body may be constructed after cancellation, slot or not"
            );
            assert_eq!(supervisor.stats().spawned, 1, "only the occupier ran");
        });
    }

