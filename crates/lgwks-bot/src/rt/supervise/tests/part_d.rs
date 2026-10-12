    /// A group signal that answers whatever the seeded world says.
    #[cfg(all(unix, feature = "process"))]
    struct WorldSignaller(WorldGroup);

    #[cfg(all(unix, feature = "process"))]
    impl GroupSignaller for WorldSignaller {
        fn signal(&self, _group: i32) -> std::io::Result<()> {
            match self.0 {
                WorldGroup::Absent => Err(std::io::Error::from_raw_os_error(ESRCH)),
                WorldGroup::Present => Err(std::io::Error::from_raw_os_error(EPERM)),
                WorldGroup::Refused => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "injected refusal",
                )),
                WorldGroup::Delivered => Ok(()),
            }
        }
    }

    /// The receipt's arm as a number, for the coverage check.
    #[cfg(all(unix, feature = "process"))]
    fn receipt_arm(receipt: &CleanupReceipt) -> u64 {
        match *receipt {
            CleanupReceipt::CleanupConfirmed => 1,
            CleanupReceipt::CleanupPending => 2,
            CleanupReceipt::CleanupFailed => 3,
            CleanupReceipt::CleanupSurvivors { .. } => 4,
        }
    }

    /// The receipt's arm and survivor count as one number, for the trace.
    ///
    /// Both, because a family's replay oracle is only meaningful if two worlds
    /// that differ only in *how many* pids survived cannot collapse into one
    /// trace.
    #[cfg(all(unix, feature = "process"))]
    fn receipt_code(receipt: &CleanupReceipt) -> Result<u64, std::num::TryFromIntError> {
        Ok(match *receipt {
            CleanupReceipt::CleanupSurvivors { ref survivors } => {
                u64::try_from(survivors.len())?.saturating_add(4)
            }
            _ => receipt_arm(receipt),
        })
    }

    /// Drive one seeded world through the real drain and check it against the model.
    #[cfg(all(unix, feature = "process"))]
    fn sim_drain(seed: u64) -> Result<u64, Box<dyn std::error::Error>> {
        let world = World::draw(seed)?;
        let at = format!("seed {seed:#018x}");
        let capture = world.capture();
        let signaller = world.signaller();
        let observer = sequence(world.present_probes);
        let mut group = capture_group(&signaller, &observer, &capture);
        if world.exited {
            group.mark_exited();
        }
        let pinned = block_on(group.cleanup());
        assert_eq!(
            pinned, world.expected,
            "{at}: the pinned phase must report the model's receipt"
        );
        assert_eq!(
            *group.containment(),
            world.containment,
            "{at}: the containment report must carry the model's counts and limits"
        );
        assert_eq!(
            capture.signals(),
            world.containment.signalled(),
            "{at}: the drain must deliver exactly the signals the model counts"
        );
        if world.exited {
            assert_eq!(
                capture.reads(),
                0,
                "{at}: a leader that had exited has no tree to walk, so no table is read"
            );
            assert!(
                !group.containment().is_complete(),
                "{at}: a cleanup that never saw the tree must not claim it"
            );
        }
        // The post-reap pass decides the group, and a survivor is never promoted.
        group.mark_reaped();
        let settled = block_on(group.confirm_absence());
        let expected = world.settled();
        assert_eq!(
            settled, expected,
            "{at}: only an observed absence settles the group, and a survivor is never \
             promoted to a clean cleanup"
        );
        let captured = u64::try_from(group.containment().captured())?;
        let signalled = u64::try_from(group.containment().signalled())?;
        let pinned_code = receipt_code(&pinned)?;
        let settled_code = receipt_code(&settled)?;
        Ok(pinned_code
            .wrapping_add(captured)
            .wrapping_add(signalled)
            .wrapping_add(settled_code))
    }

    #[cfg(all(unix, feature = "process"))]
    /// The seeds this family replays, named so a failure names one.
    const DRAIN_SEEDS: [u64; 12] = [
        0x5EED_2630_0000_0001,
        0x5EED_2630_0000_0002,
        0x5EED_2630_0000_0003,
        0x5EED_2630_0000_0004,
        0x5EED_2630_0000_0005,
        0x5EED_2630_0000_0006,
        0x5EED_2630_0000_000E,
        0x5EED_2630_0000_0019,
        0x5EED_2630_0000_0025,
        0x5EED_2630_FFFF_FFFF,
        0xDEAD_BEEF_0263_0001,
        0xC0FF_EE00_2630_0001,
    ];

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn sim_a_seeded_drain_reaches_the_models_receipt() -> Result<(), Box<dyn std::error::Error>> {
        for seed in DRAIN_SEEDS {
            sim_drain(seed)?;
        }
        Ok(())
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn sim_the_same_seed_replays_the_same_drain_trace() -> Result<(), Box<dyn std::error::Error>> {
        for seed in DRAIN_SEEDS {
            assert_eq!(
                sim_drain(seed)?,
                sim_drain(seed)?,
                "seed {seed:#018x}: the same seed must drive the same world and the same report"
            );
        }
        Ok(())
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn sim_every_receipt_arm_is_reachable_in_the_family() -> Result<(), Box<dyn std::error::Error>>
    {
        let arms = DRAIN_SEEDS
            .iter()
            .map(|seed| World::draw(*seed).map(|world| receipt_arm(&world.expected)))
            .collect::<Result<Vec<u64>, _>>()?;
        for arm in [1_u64, 2, 3, 4] {
            assert!(
                arms.contains(&arm),
                "the family must reach receipt arm {arm}; it drew {arms:?} over {} seeds",
                DRAIN_SEEDS.len()
            );
        }
        Ok(())
    }

    /// How many consecutive seeds the exited-leader sweep draws.
    #[cfg(all(unix, feature = "process"))]
    const EXITED_SWEEP_SEEDS: u64 = 4_096;

    /// Every world in a wide seeded sweep reaches the model, and the sweep holds
    /// both leaders that exited on their own and leaders that did not, under
    /// every group answer, so the exited arm is exercised against each receipt
    /// rather than once.
    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn sim_an_exited_leader_reads_no_table_and_claims_no_tree()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut exited_arms = std::collections::BTreeSet::new();
        let mut alive = 0_u64;
        for offset in 0..EXITED_SWEEP_SEEDS {
            let seed = 0x5EED_0347_0000_0000_u64.wrapping_add(offset);
            sim_drain(seed)?;
            let world = World::draw(seed)?;
            if world.exited {
                exited_arms.insert(receipt_arm(&world.expected));
            } else {
                alive = alive.saturating_add(1);
            }
        }
        assert_eq!(
            exited_arms,
            std::collections::BTreeSet::from([1_u64, 2, 3]),
            "an exited leader must be driven to the confirmed, pending and failed receipts; \
             it can never have a named survivor, because nothing was named"
        );
        assert!(
            alive > 0,
            "the sweep must still hold leaders that were alive at the cleanup"
        );
        Ok(())
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn sim_distinct_seeds_drive_distinct_drains() -> Result<(), Box<dyn std::error::Error>> {
        let [first, second, ..] = DRAIN_SEEDS;
        assert_ne!(
            sim_drain(first)?,
            sim_drain(second)?,
            "seeds {first:#018x} and {second:#018x} must draw different worlds"
        );
        Ok(())
    }

    #[test]
    fn an_over_long_panic_message_is_truncated_not_dropped() {
        let long = "x".repeat(MAX_PANIC_MESSAGE_CHARS.saturating_add(1));
        let kept = truncate_panic_message(&long);
        assert_eq!(
            kept.chars().count(),
            MAX_PANIC_MESSAGE_CHARS.saturating_add(1),
            "a truncated message keeps the cap plus the single-character marker"
        );
        assert!(
            kept.ends_with('…'),
            "a truncated message says so, so it is never read as complete"
        );
        assert_eq!(
            truncate_panic_message("short"),
            "short",
            "a message within the cap is carried unchanged"
        );
    }

    /// A grant that lands in the same instant as a cancellation is handed back
    /// through the round, so the tenant it was charged to is not left holding an
    /// admission it does not have.
    ///
    /// The ordering this test creates is the whole point and it is created, not
    /// raced. On a current-thread runtime the task that ends is queued by
    /// `notify_waiters` **before** the parked admission is queued by `cancel`,
    /// because the helper body performs both in one poll with no await between
    /// them. The permit therefore reaches the parked waiter's slot *after* the
    /// token is cancelled, and the wait is woken with a permit in hand: the one
    /// state in which a bare permit would leak the tenant's in-flight charge.
    #[cfg(feature = "script")]
    #[test]
    fn a_cancelled_admission_hands_its_granted_permit_back_to_the_round()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::rt::sync::Notify;
        use crate::rt::tenancy::TenancyPolicy;
        use crate::script::Tenant;

        block_on(async {
            // Two permits and a ceiling of one per tenant: the occupier holds
            // one, the helper takes the other, and the pool still has to be able
            // to hand the occupier's permit to the waiter below.
            let mut supervisor = Supervisor::with_tenancy(2, TenancyPolicy::new(1, 8));
            let shell = Arc::clone(
                supervisor
                    .tenancy
                    .as_ref()
                    .ok_or("a policy must install a tenancy shell")?,
            );
            let tenant = Tenant::new("held")?;
            let helper = Tenant::new("helper")?;
            let gate = Arc::new(Notify::new());
            let parked = Arc::new(AtomicUsize::new(0));
            let root = supervisor.token.clone();

            // The occupier: `tenant` takes its one admission and parks.
            supervisor
                .spawn_for(&tenant, {
                    let gate = Arc::clone(&gate);
                    let parked = Arc::clone(&parked);
                    move |_token| async move {
                        parked.fetch_add(1, Ordering::SeqCst);
                        gate.notified().await;
                    }
                })
                .await
                .map_err(|refusal| format!("the occupier was refused: {refusal}"))?;
            assert!(
                yield_until(|| parked.load(Ordering::SeqCst) >= 1, 1_000).await,
                "the occupier never parked, so the premise of this test did not hold"
            );

            // The helper: waits until the claim below has actually parked in the
            // round, then ends the occupier and cancels, with no await between
            // the two so the ordering above is the one the executor follows.
            let helper_gate = Arc::clone(&gate);
            let helper_shell = Arc::clone(&shell);
            let helper_tenant = tenant.clone();
            let helper_root = root.clone();
            supervisor
                .spawn_for(&helper, move |_token| async move {
                    while helper_shell.counts_of(&helper_tenant).1 == 0 {
                        yield_now().await;
                    }
                    helper_gate.notify_waiters();
                    helper_root.cancel();
                })
                .await
                .map_err(|refusal| format!("the helper was refused: {refusal}"))?;

            let outcome = supervisor.claim_tenanted(&shell, &tenant).await;
            assert_eq!(
                outcome.err(),
                Some(SpawnRefused::Cancelled),
                "a supervisor cancelled while a grant was in flight refuses the admission"
            );
            assert!(
                settle(&mut supervisor).await,
                "the occupier and the helper must both finish"
            );
            let (in_flight, queued) = shell.counts_of(&tenant);
            assert_eq!(
                (in_flight, queued),
                (0, 0),
                "the permit the round had already charged to {tenant} must come back \
                 through the round; a bare permit returns to the pool and leaves the \
                 tenant charged for an admission it does not hold ({in_flight} in flight)"
            );
            assert_eq!(
                supervisor.permits.available_permits(),
                2,
                "both permits are back in the pool, so nothing leaked"
            );
            Ok::<(), Box<dyn std::error::Error>>(())
        })
    }

    /// A tenanted admission joins at most `REAP_PER_ADMISSION` finished
    /// tasks, so a tenant whose bodies end at once cannot charge its joins to
    /// the neighbour that admits next (#268's adversarial row), and the bounded
    /// reap still keeps the retained set at the in-flight ceiling.
    ///
    /// The flood is created, not raced: every attacker body has finished before
    /// the neighbour's admission, so an unbounded reap would join all of them
    /// inside that one call. The second half drives 10,000 admissions of bodies
    /// that end at once, alternating tenants, and reads the retained set after
    /// each: it never exceeds the ceiling, because every call that finds two or
    /// more finished tasks joins two and places one.
    #[cfg(feature = "script")]
    #[test]
    fn a_tenanted_admission_joins_a_bounded_backlog_and_keeps_the_set_at_the_ceiling()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::REAP_PER_ADMISSION;
        use crate::rt::tenancy::TenancyPolicy;
        use crate::script::Tenant;

        const LIMIT: usize = 64;
        const FLOOD: usize = 32;
        const ADMISSIONS: usize = 10_000;
        // The bound is only a bound if it is smaller than the flood: an
        // unbounded reap joins all `FLOOD` below, so a constant at or past it
        // fails this test at compile time rather than passing vacuously.
        const _: () = assert!(
            REAP_PER_ADMISSION < FLOOD,
            "a per-admission reap must be smaller than the flood it bounds"
        );

        block_on(async {
            let mut supervisor = Supervisor::with_tenancy(LIMIT, TenancyPolicy::new(FLOOD, LIMIT));
            let attacker = Tenant::new("attacker")?;
            let neighbour = Tenant::new("neighbour")?;
            let ended = Arc::new(AtomicUsize::new(0));
            for _ in 0..FLOOD {
                let ended = Arc::clone(&ended);
                supervisor
                    .spawn_for(&attacker, move |_token| async move {
                        ended.fetch_add(1, Ordering::SeqCst);
                    })
                    .await
                    .map_err(|refusal| format!("a flood body was refused: {refusal}"))?;
            }
            assert!(
                yield_until(|| ended.load(Ordering::SeqCst) >= FLOOD, 10_000).await,
                "the flood bodies never ended, so the premise of this test did not hold"
            );
            // Let every ended body's task complete in the set, not just run.
            for _ in 0..FLOOD {
                yield_now().await;
            }
            let before = supervisor.stats().succeeded;
            supervisor
                .spawn_for(&neighbour, |_token| async {})
                .await
                .map_err(|refusal| format!("the neighbour was refused: {refusal}"))?;
            let joined = supervisor.stats().succeeded.saturating_sub(before);
            assert!(
                joined <= u64::try_from(REAP_PER_ADMISSION)?,
                "the neighbour's admission joined {joined} of the flood's finished tasks; \
                 at most {REAP_PER_ADMISSION} may land on its critical path"
            );

            let mut largest = 0_usize;
            for admission in 0..ADMISSIONS {
                let tenant = if admission % 2 == 0 {
                    &attacker
                } else {
                    &neighbour
                };
                supervisor
                    .spawn_for(tenant, |_token| async {})
                    .await
                    .map_err(|refusal| format!("admission {admission} was refused: {refusal}"))?;
                largest = largest.max(supervisor.set.len());
                yield_now().await;
            }
            assert!(
                largest <= LIMIT,
                "the retained set reached {largest} tasks against an in-flight ceiling of {LIMIT}"
            );
            let report = supervisor.shutdown().await;
            assert_eq!(
                report.stats.succeeded,
                u64::try_from(FLOOD + 1 + ADMISSIONS)?,
                "every body is joined and counted by shutdown"
            );
            Ok(())
        })
    }

    /// A parked admission keeps its place in its tenant's FIFO queue, and leaves
    /// nothing abandoned behind it, however long it waits.
    ///
    /// Three waiters on one tenant, because FIFO order is only observable with
    /// company: a queue of one is in order under every implementation. The
    /// claim below is driven by the real `claim_tenanted` and is parked with the
    /// poll interval the old loop used, so a waiter that was dropped and re-asked
    /// would rotate to the back of these three on every interval — which is the
    /// defect this pins.
    #[cfg(feature = "script")]
    #[test]
    fn a_parked_tenant_admission_keeps_its_place_in_the_queue()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::rt::sync::OwnedSemaphorePermit;
        use crate::rt::tenancy::TenancyPolicy;
        use crate::script::Tenant;
        use std::future::Future;
        use std::task::{Context, Poll, Waker};

        /// Slices of 50 ms. Twenty-four of them is 1.2 s, an order of magnitude
        /// past the 100 ms interval the old loop wrapped each wait in, so a
        /// waiter that was dropped and re-asked would rotate several times.
        const SLICES: u32 = 24;
        /// How long one slice of the park lasts.
        const SLICE: Duration = Duration::from_millis(50);

        block_on(async {
            // One permit, a ceiling of three and a queue of eight: the permit is
            // held back by the test, so every arrival parks rather than being
            // admitted, and the three waiters share one tenant's queue.
            let mut supervisor = Supervisor::with_tenancy(1, TenancyPolicy::new(3, 8));
            let shell = Arc::clone(
                supervisor
                    .tenancy
                    .as_ref()
                    .ok_or("a policy must install a tenancy shell")?,
            );
            let pool = Arc::clone(&supervisor.permits);
            let tenant = Tenant::new("queued")?;
            // The pool's one permit is held back by the test before the first
            // arrival, so every arrival below parks rather than being admitted.
            let mut held = Some(match Arc::clone(&pool).try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => return Err("the pool did not start with its one permit".into()),
            });

            // The claim under test registers first, so FIFO serves it first. It
            // borrows the supervisor for as long as it is parked, which is why
            // the pool is cloned out before the pin and the two company waiters
            // are registered through the shell rather than through the
            // supervisor.
            let mut claim = std::pin::pin!(supervisor.claim_tenanted(&shell, &tenant));
            let mut probe = Context::from_waker(Waker::noop());
            assert!(
                claim.as_mut().poll(&mut probe).is_pending(),
                "the claim must park on a spent pool rather than be admitted"
            );
            let mut second = queued_waiter(&shell, &tenant)?;
            let mut third = queued_waiter(&shell, &tenant)?;

            let mut slices = 0_u32;
            while slices < SLICES {
                assert!(
                    crate::rt::time::timeout(SLICE, claim.as_mut())
                        .await
                        .is_err(),
                    "the claim cannot resolve while the pool is spent"
                );
                assert_eq!(
                    shell.counts_of(&tenant).1,
                    3,
                    "all three waiters stay live while they are parked"
                );
                assert_eq!(
                    shell.retained_waiters(&tenant),
                    3,
                    "a waiter that keeps its place leaves no abandoned entry behind; \
                     the deque grew to {} entries over {slices} polls",
                    shell.retained_waiters(&tenant)
                );
                slices = slices.saturating_add(1);
            }

            // Serve them in turn. One permit is handed to the round, the waiter
            // the round names resolves, and the permit it was holding is handed
            // straight on to the next — so the order the three resolve in is the
            // queue's own order and nothing else.
            let mut order: Vec<u8> = Vec::new();
            // Hoisted out of the loop: a future that has returned must never be
            // polled again, and the claim is the one that resolves first.
            let mut claim_out: Option<Result<Lease, SpawnRefused>> = None;
            let mut claim_done = false;
            for _ in 0..3 {
                if let Some(permit) = held.take() {
                    shell.release(&tenant, permit);
                }
                let mut second_out: Option<OwnedSemaphorePermit> = None;
                let mut third_out: Option<OwnedSemaphorePermit> = None;
                crate::rt::time::timeout(
                    Duration::from_secs(5),
                    std::future::poll_fn(|context| {
                        if !claim_done
                            && claim_out.is_none()
                            && let Poll::Ready(value) = claim.as_mut().poll(context)
                        {
                            claim_done = true;
                            claim_out = Some(value);
                        }
                        if second_out.is_none()
                            && let Poll::Ready(permit) = second.as_mut().poll(context)
                        {
                            second_out = Some(permit);
                        }
                        if third_out.is_none()
                            && let Poll::Ready(permit) = third.as_mut().poll(context)
                        {
                            third_out = Some(permit);
                        }
                        if claim_done || second_out.is_some() || third_out.is_some() {
                            return Poll::Ready(());
                        }
                        Poll::Pending
                    }),
                )
                .await
                .map_err(|_| "no waiter resolved after a permit was released into the round")?;
                let index = if claim_out.is_some() {
                    match claim_out.take() {
                        Some(Ok(lease)) => {
                            // The granted lease hands its permit to the next
                            // waiter as it drops, which is the hand-off the next
                            // round needs.
                            drop(lease);
                            0
                        }
                        Some(Err(refusal)) => {
                            return Err(format!("the parked claim was refused: {refusal}").into());
                        }
                        None => return Err("the claim reported no outcome at all".into()),
                    }
                } else if let Some(permit) = second_out {
                    held = Some(permit);
                    1
                } else if let Some(permit) = third_out {
                    held = Some(permit);
                    2
                } else {
                    return Err("a resolution was reported with no waiter behind it".into());
                };
                order.push(index);
            }
            assert_eq!(
                order,
                vec![0, 1, 2],
                "one tenant's three waiters are served in arrival order: the claim \
                 that parked first, then the two behind it"
            );
            assert_eq!(
                shell.retained_waiters(&tenant),
                0,
                "every waiter left the queue once it was served"
            );
            Ok::<(), Box<dyn std::error::Error>>(())
        })
    }

    /// A waiting spawn keeps its place in the pool's own fair queue.
    ///
    /// Two supervisors share ONE pool of a single permit, built through the
    /// private `assembled` constructor, so the queue under test is the pool's
    /// and both waiters can be parked at once — a supervisor's `&mut self` is
    /// what stops two of its own waiters from existing, not the semaphore's
    /// fairness.
    ///
    /// The timing is the assertion, and it is arithmetic rather than hope. The
    /// first claim registers at t = 0; the second registers at t = 90, so under
    /// the old loop the first claim's acquire expires at t = 100 and it re-queues
    /// *behind* the second, leaving the pool's queue ordered second-then-first
    /// for the next ninety milliseconds. The release sits at t = 140, inside
    /// that ninety-millisecond window and two orders of magnitude clear of
    /// either end of it.
    #[test]
    fn a_waiting_spawn_keeps_its_place_in_the_pool_queue() -> Result<(), Box<dyn std::error::Error>>
    {
        /// When the second claim starts being polled, ninety milliseconds in.
        const REGISTER_AT: Duration = Duration::from_millis(90);
        /// When the pool's only permit goes back, half a period after the first
        /// claim's acquire would have expired.
        const RELEASE_AT: Duration = Duration::from_millis(150);
        /// A ceiling on a failure, not a budget the assertion must finish inside.
        const LIMIT: Duration = Duration::from_secs(2);

        block_on(async {
            let pool = Arc::new(crate::rt::sync::Semaphore::new(1));
            let mut first = Supervisor::assembled(1, Arc::clone(&pool), Clock::wall());
            let mut second = Supervisor::assembled(1, Arc::clone(&pool), Clock::wall());
            let mut held = match Arc::clone(&pool).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => return Err("the shared pool did not start with its one permit".into()),
            };

            let mut first_claim = std::pin::pin!(first.claim());
            let mut second_claim = std::pin::pin!(second.claim());
            let mut first_out: Option<Option<Lease>> = None;
            let mut second_out: Option<Option<Lease>> = None;

            // The driver is a real-clock spin rather than a chain of timed waits,
            // because the assertion is about two events a hundred milliseconds
            // apart: the first claim's acquire expiring, and the permit being
            // released. Timed slices drift by a millisecond or two each, and a
            // driver that drifts cannot place a release inside a window it is
            // trying to hit.
            let started = std::time::Instant::now();
            let mut second_started = false;
            let mut released = false;
            std::future::poll_fn(|context| {
                let now = started.elapsed();
                if !second_started && now >= REGISTER_AT {
                    second_started = true;
                }
                if first_out.is_none()
                    && let Poll::Ready(value) = first_claim.as_mut().poll(context)
                {
                    first_out = Some(value);
                }
                if second_started
                    && second_out.is_none()
                    && let Poll::Ready(value) = second_claim.as_mut().poll(context)
                {
                    second_out = Some(value);
                }
                if !released && now >= RELEASE_AT {
                    if let Some(permit) = held.take() {
                        // The permit goes back to the pool, and the pool hands it
                        // to the head of its own fair queue.
                        drop(permit);
                    }
                    released = true;
                }
                if first_out.is_some() || second_out.is_some() || now >= LIMIT {
                    return Poll::Ready(());
                }
                // Re-poll immediately: the driver is measuring elapsed real time
                // and has nothing to wait on.
                context.waker().wake_by_ref();
                Poll::Pending
            })
            .await;

            assert!(
                released,
                "the driver never reached the release moment within {LIMIT:?}"
            );
            assert!(
                first_out.is_some(),
                "the caller that arrived first never received the released permit, so \
                 its place in the pool's queue was forfeited; the caller that arrived \
                 second was served instead (second resolved: {})",
                second_out.is_some()
            );
            assert!(
                second_out.is_none(),
                "the permit went to the caller that arrived second: a waiter that keeps \
                 its place is served in arrival order"
            );
            Ok::<(), Box<dyn std::error::Error>>(())
        })
    }

    /// Park one extra waiter on `tenant`, or report which arm answered instead.
    ///
    /// A helper rather than three inline matches: the second and third waiter
    /// are identical constructions, and a copy per waiter is how two of them end
    /// up arming a different arm than the first.
    #[cfg(feature = "script")]
    fn queued_waiter(
        shell: &Arc<super::tenancy_support::TenancyShell>,
        tenant: &crate::script::Tenant,
    ) -> Result<std::pin::Pin<Box<super::tenancy_support::WaitPermit>>, Box<dyn std::error::Error>>
    {
        match shell.admit(tenant) {
            super::tenancy_support::Admission::Queued(waiter) => Ok(Box::pin(waiter)),
            super::tenancy_support::Admission::Admitted(_) => {
                Err("the pool was spent, so this arrival must have parked".into())
            }
            super::tenancy_support::Admission::Refused { limit } => {
                Err(format!("the queue bound of {limit} was already reached").into())
            }
            super::tenancy_support::Admission::SupervisorQueueFull { limit } => {
                Err(format!("the supervisor's waiting bound of {limit} was already reached").into())
            }
        }
    }

    #[test]
    fn wait_idle_joins_every_task_without_cancelling_and_leaves_the_supervisor_usable() {
        block_on(async {
            let ran = Arc::new(AtomicU64::new(0));
            let mut supervisor = Supervisor::new(3);
            for _ in 0..40 {
                let ran = Arc::clone(&ran);
                supervisor
                    .spawn(move |token| async move {
                        yield_now().await;
                        if !token.is_cancelled() {
                            ran.fetch_add(1, Ordering::SeqCst);
                        }
                    })
                    .await;
            }
            supervisor.wait_idle().await;
            let stats = supervisor.stats();
            assert_eq!(stats.in_flight(), 0, "nothing is left in flight");
            assert_eq!(stats.succeeded, 40, "every task completed, none cancelled");
            assert_eq!(stats.cancelled, 0, "waiting is not stopping");
            assert_eq!(ran.load(Ordering::SeqCst), 40, "every body ran to its end");
            assert!(
                !supervisor.is_cancelled(),
                "the supervisor was not cancelled"
            );

            // Still usable: the wait did not consume the supervisor.
            supervisor.spawn(|_token| async {}).await;
            assert_eq!(
                supervisor.wait_idle().await,
                1,
                "the next wait joins the next task"
            );
            assert_eq!(
                supervisor.wait_idle().await,
                0,
                "an idle supervisor joins nothing"
            );
        });
    }

    #[test]
    fn wait_idle_counts_reports_past_the_retention_cap_rather_than_losing_them() {
        block_on(async {
            let mut supervisor = Supervisor::new(2);
            for _ in 0..2 {
                supervisor.spawn(|_token| async {}).await;
            }
            // Fill the pool without reaping, so `wait_idle` has more outcomes
            // to absorb than the cap retains.
            supervisor.wait_idle().await;
            for _ in 0..2 {
                supervisor.spawn(|_token| async {}).await;
            }
            supervisor.wait_idle().await;
            let stats = supervisor.stats();
            let retained = drain(&mut supervisor).len();
            assert_eq!(
                u64::try_from(retained)
                    .ok()
                    .map(|kept| kept.saturating_add(stats.reports_dropped)),
                Some(4),
                "every outcome is either retained or counted as dropped"
            );
        });
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn a_spawn_waiting_on_a_cleanup_owned_permit_is_admitted_once_the_owner_settles()
    -> Result<(), Box<dyn std::error::Error>> {
        block_on(async {
            let mut supervisor = Supervisor::new(1);
            let Some(permit) = supervisor.try_take() else {
                return Err("a fresh supervisor of bound one has its permit".into());
            };
            // A group id above any pid limit: the native probe reports it absent,
            // so the owner settles on the first reap that drives it — which only
            // the waiting spawn's recheck performs.
            let task = TaskId(u64::MAX);
            supervisor.cleanup_owners.register(
                task,
                0x7fff_fff0,
                Lease::plain(permit),
                whole_capture(),
            );
            let admitted = crate::rt::time::timeout(
                Duration::from_secs(5),
                supervisor.spawn(|_token| async {}),
            )
            .await;
            if admitted.is_err() {
                return Err("the spawn never left the bound: the recheck did not release the owner's permit".into());
            }
            supervisor.wait_idle().await;
            let settled = drain(&mut supervisor)
                .into_iter()
                .any(|outcome| matches!(outcome, TaskOutcome::CleanupSettled { task: settled, .. } if settled == task));
            if !settled {
                return Err("the owner's settlement was not reported".into());
            }
            assert_eq!(supervisor.stats().succeeded, 1, "the waiting body ran");
            Ok(())
        })
    }
