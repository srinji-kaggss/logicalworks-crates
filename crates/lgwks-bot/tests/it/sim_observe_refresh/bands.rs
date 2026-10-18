//! The band families: forced refresh, a refresh that never lands, tenants, event identities and replay.

use super::*;

// ── Families ───────────────────────────────────────────────────────────────

/// Every armed cause reaches the report on the tick it was armed, and nothing
/// else does.
///
/// The property is a bijection between the schedule and the report: an armed
/// window produces a forced refresh naming that chain and that cause, and an
/// unarmed chain produces nothing. Either half alone would pass against a
/// report that listed every chain every tick, which is the defect the row
/// forbids from the other side — a bot that re-reads everything forever is
/// indistinguishable from a healthy one only if you never look at the report.
pub(super) fn forced_refresh_matches_the_schedule(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng());
        let count = usize::try_from(plan.tenants)?;
        let mut tenants = Vec::with_capacity(count);
        for index in 0..count {
            let (_, subject) = tenant_of(&plan, index)?;
            tenants.push(subject);
        }

        // Everything the run saw, as report rows.
        let mut rows: Vec<ReportRow> = Vec::new();
        for tick in 1..=plan.ticks {
            for (index, subject) in tenants.iter_mut().enumerate() {
                let schedule = plan.schedule(index)?;
                subject.arm(tick, &schedule);
                subject.advance(tick);
                subject.observe(&mut rows);
            }
        }

        // Every armed chain is reported at least once over the run, and every
        // reported forced row names a chain whose window covers the tick it was
        // reported on. Checked against the schedule rather than a count, because
        // a count is satisfied by the right number of wrong rows. Only the
        // `forced` rows are counted: a `superseded` row is a different outcome
        // and folding it in would make the two indistinguishable.
        let mut reported: Vec<String> = rows
            .iter()
            .filter(|row| row.kind == "forced")
            .map(ReportRow::spell)
            .collect();
        reported.sort();
        reported.dedup();
        for row in &rows {
            if row.kind != "forced" {
                continue;
            }
            let schedule = plan
                .tenant(usize::try_from(row.tenant)?)
                .ok_or("a report from a tenant the schedule does not declare")?;
            let window = schedule.faults.get(row.chain).copied().ok_or_else(|| {
                format!(
                    "tenant {} reported chain {} but declared {MAX_CHAINS} chains",
                    row.tenant, row.chain
                )
            })?;
            assert_eq!(
                window.cause.map(RefreshReason::as_str),
                Some(row.detail.as_str()),
                "tenant {} chain {} reported as {} but its window names {:?}",
                row.tenant,
                row.chain,
                row.detail,
                window.cause.map(RefreshReason::as_str)
            );
        }
        let armed_chains: usize = (0..usize::try_from(plan.tenants)?)
            .filter_map(|index| {
                // A tenant the schedule does not declare contributes no armed
                // chain, and the assertion above has already required every armed
                // chain to come from one it does declare.
                let schedule = plan.tenant(index)?;
                Some(
                    (0..MAX_CHAINS)
                        .filter_map(move |chain| {
                            schedule
                                .faults
                                .get(chain)?
                                .cause
                                .map(|cause| (index, chain, cause.as_str()))
                        })
                        .count(),
                )
            })
            .sum();
        let reported_chains = reported.len();
        assert_eq!(
            reported_chains, armed_chains,
            "every armed chain reached the report exactly once, and no unarmed one \
             did: the row is a bijection, not a count"
        );
        // The schedule's shape first, so a seed that armed nothing still replays
        // to a receipt of what it decided rather than to the empty trace.
        sim.record(&format!(
            "tenants={} ticks={} armed={armed_chains}",
            plan.tenants, plan.ticks
        ));
        for row in &rows {
            sim.record(&row.spell());
        }
        Ok(())
    })
}

/// A chain whose forced poll keeps refusing is forced again, on the next tick.
///
/// The property that separates "the mark is spent by the read" from "the mark is
/// spent by the *successful* read". A source that arms and then refuses every
/// poll has replaced nothing, so comparing against its old baseline would be
/// comparing against a value it has said it cannot trust — the quiet state T08
/// exists to rule out.
pub(super) fn a_refresh_that_never_lands_stays_marked(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        // A schedule that refuses everything: no draw decides it, because the
        // property needs the refusal to be certain rather than likely. Chain 0
        // is armed from tick 2 onward; chains 1 and 2 are healthy throughout,
        // which is what makes "and not the siblings" checkable.
        let schedule = TenantPlan {
            faults: [
                FaultWindow {
                    cause: Some(RefreshReason::WatchOverflow),
                    from: 2,
                    ticks: 4,
                },
                FaultWindow {
                    cause: None,
                    from: 0,
                    ticks: 0,
                },
                FaultWindow {
                    cause: None,
                    from: 0,
                    ticks: 0,
                },
            ],
            refusing: true,
            holds: false,
        };
        let mut subject = tenant(0, &schedule)?;

        // One clean tick first, so there is a baseline to be unsound about.
        let _outcome = subject.bot.tick();

        let mut rows: Vec<ReportRow> = Vec::new();
        for tick in 2..=4_u32 {
            subject.arm(tick, &schedule);
            subject.advance(tick);
            subject.observe(&mut rows);

            let report = subject.bot.tick_report();
            let forced = report
                .forced()
                .iter()
                .find(|entry| entry.chain() == 0)
                .ok_or_else(|| {
                    format!(
                        "tick {tick}: chain 0 declared an unsound baseline whose every \
                         forced poll refused, and it was not forced again"
                    )
                })?;
            assert_eq!(
                forced.reason(),
                RefreshReason::WatchOverflow,
                "tick {tick}: the cause is still the one the source declared"
            );
            assert!(
                report.forced().iter().all(|entry| entry.chain() == 0),
                "tick {tick}: the healthy sibling is not dragged in by its sibling's \
                 fault — {:?}",
                report.forced()
            );
            sim.record(&format!(
                "tick {tick} forced chain {} {}",
                forced.chain(),
                forced.reason().as_str()
            ));
        }
        for row in &rows {
            sim.record(&row.spell());
        }
        Ok(())
    })
}

/// Two tenants over one process keep separate reports, separate marks and
/// separate action logs.
///
/// Isolation is not "the second tenant built": it is that a chain in one tenant
/// never appears in the other's report, that a fault armed on one does not
/// force the other, and that the two action logs contain only their own
/// chains' work. The two tenants share a `GrantSet`, an environment and a
/// process, and differ only in their run identity — which is the closest thing
/// to a cross-tenant effect that a test can construct without a shared journal.
pub(super) fn tenants_never_cross(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng());
        let mut left = tenant(
            0,
            &plan
                .tenant(0)
                .ok_or("a two-tenant schedule with no left tenant")?,
        )?;
        let mut right = tenant(
            1,
            &plan
                .tenant(1)
                .ok_or("a two-tenant schedule with no right tenant")?,
        )?;

        let mut rows: Vec<ReportRow> = Vec::new();
        for tick in 1..=plan.ticks {
            for (subject, index) in [(&mut left, 0_usize), (&mut right, 1_usize)] {
                let schedule = plan.schedule(index)?;
                subject.arm(tick, &schedule);
                subject.advance(tick);
                subject.observe(&mut rows);
            }
        }
        for row in &rows {
            sim.record(&row.spell());
        }

        // Each tenant's report names only its own chains, and only within the
        // chain count it declared. A bot-wide mark would show up here as one
        // tenant's chain index appearing in the other's report.
        for subject in [&left, &right] {
            let declared = MAX_CHAINS;
            let report = subject.bot.tick_report();
            for forced in report.forced() {
                assert!(
                    forced.chain() < declared,
                    "tenant {} reported chain {} but declared {declared} chains",
                    subject.tenant,
                    forced.chain()
                );
                assert!(
                    forced
                        .domain()
                        .starts_with(&format!("sim::t{}", subject.tenant)),
                    "tenant {} reported a source belonging to another tenant: {}",
                    subject.tenant,
                    forced.domain()
                );
            }
        }

        // The action logs are the other half: each contains only its own chains'
        // work. A shared journal or a shared ledger would show one tenant's
        // action running the other tenant's payload.
        for subject in [&left, &right] {
            let declared = MAX_CHAINS;
            for &(chain, _) in subject.ran.borrow().iter() {
                assert!(
                    chain < declared,
                    "tenant {} ran an action at chain {chain} but declared \
                     {declared} chains — the other tenant's work reached this one",
                    subject.tenant
                );
            }
            sim.record(&format!(
                "tenant-{} ran {}",
                subject.tenant,
                subject.ran.borrow().len()
            ));
        }
        Ok(())
    })
}

/// The same payload under distinct event ids fires each time; a redelivery of
/// any id already seen fires nothing.
///
/// The seeded half of T09's identity claim. The plan draws a stream of event
/// ids over one fixed payload, some of them repeats, and the expectation is
/// derived from the stream rather than asserted as a count: every id's *first*
/// occurrence executes, and no later occurrence of any id does.
pub(super) fn event_identities_are_per_event(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        // One payload for the whole stream, so the only thing separating two
        // events is their id. A dedup keyed on the payload would fire exactly
        // one of them and the trace would show it.
        let payload = sim.rng().below(64);
        let delivered = sim.rng().between(4, 12);

        // The stream, drawn first so the queue order is fixed: the feed pops from
        // the back, so the queue is built in reverse to deliver in draw order.
        let mut stream = Vec::new();
        let mut distinct = 0_u32;
        for _ in 0..delivered {
            let reuse = !stream.is_empty() && sim.rng().chance(450);
            let id = if reuse {
                stream[usize::try_from(sim.rng().below(distinct))?]
            } else {
                distinct = distinct.saturating_add(1);
                distinct
            };
            stream.push(id);
        }
        let queue = Rc::new(RefCell::new(
            stream
                .iter()
                .rev()
                .copied()
                .map(|id| EventId::new(u64::from(id), payload))
                .collect::<Vec<_>>(),
        ));
        let ran = Rc::new(RefCell::new(Vec::new()));
        let mut bot = Bot::builder("sim-observe-events")
            .observe(Feed {
                queue: Rc::clone(&queue),
                polls: Rc::new(Cell::new(0)),
            })
            .on(
                |event: &EventId<u32>| event.id() > 0,
                LogsEvents {
                    ran: Rc::clone(&ran),
                },
            )
            .with_effects(tenant_scope(0)?)
            .build(&GrantSet::empty())?;

        for _ in 0..delivered {
            let _outcome = bot.tick();
        }

        // Every distinct id fires exactly once, and only on its first delivery. The
        // expectation is the *unique* ids in first-seen order, which is what
        // `Vec::dedup` does not give: it removes consecutive duplicates only, so
        // a stream like [1, 2, 1] survives it intact. A set would lose the order,
        // which is the half that says an id fires on its *first* delivery rather
        // than whenever it was last seen.
        let mut wanted: Vec<u64> = Vec::new();
        for id in &stream {
            let id = u64::from(*id);
            if !wanted.contains(&id) {
                wanted.push(id);
            }
        }
        let fired = ran.borrow().clone();
        assert_eq!(
            fired.len(),
            wanted.len(),
            "each distinct event id fired exactly once over an identical payload: \
             {fired:?} for a stream of {stream:?}"
        );
        let mut sorted_fired = fired.clone();
        sorted_fired.sort_unstable();
        let mut sorted_wanted = wanted.clone();
        sorted_wanted.sort_unstable();
        assert_eq!(
            sorted_fired, sorted_wanted,
            "the ids that fired are exactly the distinct ids of the stream — a \
             payload-keyed dedup would have fired one of them"
        );
        assert_eq!(
            wanted.len(),
            usize::try_from(distinct)?,
            "the drawn stream and the drawn distinct count agree, so the schedule \
             and the expectation are one fact rather than two"
        );
        assert!(
            distinct > 0,
            "the stream must contain at least one distinct event id, or the run \
             proves nothing"
        );
        sim.record(&format!(
            "events delivered={delivered} distinct={distinct} fired={}",
            fired.len()
        ));
        Ok(())
    })
}

/// The same seed produces the same trace hash, twice over.
///
/// The tier sweep lives in `observe_refresh.rs`, not here. It has to: the
/// saturation tiers need 100, 1,000 and 10,000 chains on one bot, and
/// `Bot::observe` returns a builder whose type follows its source — so a bot of
/// *n* chains needs *n* written-out `observe`/`on` pairs, which is a compile-time
/// constant and not something a test can express at runtime. This family covers
/// the fault dimension a fixed chain count is the right shape for: which chain
/// declares what, over what window, and whether the report says so.
///
/// The body is the union of the families above, so the hash covers forced
/// refreshes, pass-overs, per-tenant reports and the action logs rather than one
/// scenario's shape. `assert_replays` sweeps the band twice and compares the two
/// hash vectors, so a nondeterministic run fails even when every assertion
/// passes.
pub(super) fn the_same_seed_replays(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng());
        let count = usize::try_from(plan.tenants)?;
        for index in 0..count {
            let (tenant_id, mut subject) = tenant_of(&plan, index)?;
            let schedule = plan.schedule(index)?;
            let mut rows: Vec<ReportRow> = Vec::new();
            for tick in 1..=plan.ticks {
                subject.arm(tick, &schedule);
                subject.advance(tick);
                subject.observe(&mut rows);
            }
            for row in &rows {
                sim.record(&row.spell());
            }
            // The action log goes into the trace in order, never a timing.
            for &(chain, value) in subject.ran.borrow().iter() {
                sim.record(&format!("ran {tenant_id} {chain} {value}"));
            }
            // So does the held work, which is the part of the run a quiet tick
            // would otherwise hide.
            for work in subject.bot.pending() {
                sim.record(&format!("held {tenant_id} {}", work.id().chain()));
            }
        }
        // Settling what is held is what turns the ledger from "held" into
        // "decided", and it is where a cross-tenant mistake would show up: a
        // settlement for another tenant's key is a typed refusal, not a quiet
        // success.
        for index in 0..count {
            let (tenant_id, mut subject) = tenant_of(&plan, index)?;
            subject.advance(0);
            let _outcome = subject.bot.tick();
            for work in subject.bot.pending() {
                let Some(key) = work.key() else {
                    continue;
                };
                match subject.bot.resolve_effect(&key, EffectEvidence::Applied) {
                    Ok(()) => sim.record(&format!("settled {tenant_id}")),
                    Err(error) => sim.record(&format!("settle-refused {tenant_id} {error:?}")),
                }
            }
        }
        Ok(())
    })
}
