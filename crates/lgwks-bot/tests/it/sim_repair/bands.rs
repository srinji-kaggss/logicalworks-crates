//! The seeded families the band table drives: order independence, tenant isolation and ticket saturation.

use super::*;

// ── Families ───────────────────────────────────────────────────────────────

/// One decision of a seeded order: apply it, then check the run was charged only what the decision allows.
fn decision_in_order(
    sim: &mut sim::Sim,
    case: &Case,
    plan: &Plan,
    index: usize,
    action: &Action,
) -> TestResult {
    let Case {
        ref host,
        ref ticket,
        run,
        ref declared,
        ref polls,
    } = *case;
    let before = shared::ledger_of(host)?
        .control(run)
        .ok_or("the blocked run was charged")?;
    let verdict = decide(host, declared, ticket, polls, *action, plan.reach)?;
    let after = shared::ledger_of(host)?
        .control(run)
        .ok_or("the run still has a control state")?;
    match *action {
        Action::Repair => {
            // A repair succeeds while the ticket is unused and is refused
            // once it has been used — and which of those this is depends on
            // whether an earlier decision in *this* order already took it.
            // Both are correct; what must hold either way is that the run
            // never ends past one applied ticket.
            assert!(
                verdict == "succeeded" || after.epoch() <= 1,
                "decision {index} ({}) left the run at epoch {} with {} applied",
                action.label(),
                after.epoch(),
                after.applied()
            );
        }
        Action::Deny | Action::OverWide => {
            assert_eq!(
                (after.attempts(), after.spend(), after.epoch()),
                (before.attempts(), before.spend(), before.epoch()),
                "decision {index} ({}) must charge nothing and mint no epoch",
                action.label()
            );
            assert_eq!(
                verdict,
                "refused",
                "decision {index} ({}) is a denial",
                action.label()
            );
        }
        Action::Duplicate => {
            // A redelivery applies the ticket if nothing has yet and is
            // refused if something has; either way it moves the epoch at
            // most once, and never past one. The bound is what the
            // property is, so it is asserted rather than predicted from
            // the action list — which action came first is the seed's
            // business, and a test that read it back would prove the plan
            // rather than the run.
            assert!(
                after.epoch() <= 1,
                "a redelivery at step {index} left the run at epoch {}, which is \
                 past the one ticket it has",
                after.epoch()
            );
        }
        Action::Inspect => {}
    }
    sim.record(&format!(
        "decision {index} {} verdict={verdict} attempts={} spend={} epoch={} applied={}",
        action.label(),
        after.attempts(),
        after.spend(),
        after.epoch(),
        after.applied()
    ));
    Ok(())
}

/// The order of repair / duplicate / stale / deny is immaterial — and the same
/// seed replays exactly.
///
/// Both properties in one family because they are one run: `assert_replays`
/// sweeps the band twice and compares the trace hashes, so the determinism receipt
/// and the order-independence claim come from the same pass rather than from two
/// families that share a skeleton and could drift into testing two different
/// things under one name. The property the whole file exists for is that whatever
/// order a seed draws, a ticket applies **at most once**, and a run that has had
/// one ticket applied is at epoch one with one applied ticket however many times
/// it was delivered.
pub(super) fn seeded_orders_reach_the_same_state(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng())?;
        let dir = sim.scratch("repair-order")?;
        let case = block(TENANTS[0], &dir, plan.reach)?;
        let Case {
            ref host,
            ticket: _,
            run,
            declared: _,
            ref polls,
        } = case;

        for (index, action) in plan.actions.iter().enumerate() {
            decision_in_order(sim, &case, &plan, index, action)?;
        }

        // The endpoint does not depend on the order. Two things are true whatever
        // order the seed drew: the epoch and the applied count agree (both zero,
        // or both one), and neither ever exceeds one — however many deliveries
        // were attempted.
        let end = shared::ledger_of(host)?
            .control(run)
            .ok_or("the run still has a control state")?;
        assert!(
            end.epoch() <= 1,
            "however the decisions were ordered, the run is at epoch {} and no \
             further: one ticket applies at most once",
            end.epoch()
        );
        assert_eq!(
            end.epoch(),
            u64::try_from(end.applied())?,
            "the epoch and the applied-ticket count are one fact: a run that had a \
             ticket applied is at epoch one, and one that did not is at zero"
        );
        assert_eq!(
            polls.analysis(),
            1,
            "the analysis is recorded once however many decisions are taken"
        );
        assert!(
            polls.publish() <= 1,
            "the publication runs at most once: {} deliveries produced {} publications",
            plan.actions.len(),
            polls.publish()
        );
        sim.record(&format!(
            "ordinal={} reach={} decisions={} analysis={} publish={} admitted={} refused={} peak={}",
            plan.ordinal,
            plan.reach,
            plan.actions.len(),
            polls.analysis(),
            polls.publish(),
            host.admission().admitted(),
            host.admission().refused(),
            host.admission().peak_in_flight()
        ));
        Ok(())
    })
}

/// Two tenants over one directory: a run, a ticket, an epoch and a budget never
/// cross.
///
/// Both tenants run the same task with the same need and the same directory, so
/// a ledger or a ticket that was not tenant-scoped would be found here: the step
/// keys hash the tenant, and the repair ledger is a file per tenant.
pub(super) fn tenants_keep_their_own_tickets_and_budgets(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng())?;
        let dir = sim.scratch("repair-tenants")?;

        let mut runs = Vec::with_capacity(TENANTS.len());
        for tenant in TENANTS {
            let case = block(tenant, &dir, plan.reach)?;
            let verdict = decide(
                &case.host,
                &case.declared,
                &case.ticket,
                &case.polls,
                Action::Repair,
                plan.reach,
            )?;
            assert_eq!(
                verdict, "succeeded",
                "{tenant}: its own repair is authorized"
            );
            runs.push(Answered {
                tenant,
                run: case.run,
                ticket: case.ticket,
                host: case.host,
                declared: case.declared,
                polls: case.polls,
            });
        }

        let mut seen_ids = Vec::with_capacity(TENANTS.len());
        for entry in &runs {
            assert_eq!(
                entry.ticket.tenant(),
                entry.tenant,
                "a ticket names its own tenant"
            );
            seen_ids.push(entry.run);
            let control = shared::ledger_of(&entry.host)?
                .control(entry.run)
                .ok_or("the charged run has a control state")?;
            assert_eq!(
                control.tenant(),
                entry.tenant,
                "the ledger attributes the run to the tenant that made it"
            );
            assert_eq!(
                (control.epoch(), control.applied()),
                converged(1),
                "{}: one repair, one epoch, one applied ticket",
                entry.tenant
            );
            assert_eq!(
                entry.polls.analysis(),
                1,
                "{}: its own analysis ran once",
                entry.tenant
            );
        }
        assert_ne!(
            seen_ids[0], seen_ids[1],
            "two tenants minting runs over one directory must not collide, or one \
             tenant's repair would answer the other's ticket"
        );

        // Cross-tenant: one tenant's ticket is refused by the other tenant's host.
        let (mine, theirs) = (&runs[0], &runs[1]);
        match lgwks_bot::block_on(theirs.host.repair(
            &mine.ticket,
            &exact_grant(plan.reach),
            &mine.declared,
            input(Arc::clone(&mine.polls), 7),
            1,
        )) {
            Err(RepairError::ForeignTenant {
                ref ticket,
                ref host,
            }) => {
                assert_eq!(
                    (ticket.as_str(), host.as_str()),
                    (mine.tenant, theirs.tenant),
                    "the refusal names both tenants, so a reader can see which is which"
                );
            }
            other => {
                let refusal =
                    Err(format!("another tenant's ticket must be refused, got {other:?}").into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "tenants_keep_their_own_tickets_and_budgets: returning an error to the caller");
                return refusal;
            }
        }
        // The run ids are *not* in the trace: they are minted from the estate's
        // entropy source and differ between two sweeps of one seed, which would
        // make the replay receipt a coin toss. The two tenants' positions and their
        // control states are what the family asserts, and those are facts.
        sim.record(&format!(
            "tenants={} reach={} distinct-ids={} admitted={:?} epochs={:?}",
            TENANTS.len(),
            plan.reach,
            seen_ids.len(),
            runs.iter()
                .map(|entry| entry.host.admission().admitted())
                .collect::<Vec<_>>(),
            runs.iter()
                .map(|entry| {
                    shared::ledger_of(&entry.host)
                        .and_then(|ledger| {
                            ledger.control(entry.run).ok_or_else(|| {
                                shared::missing("the charged run has a control state")
                            })
                        })
                        .map(|control| (control.epoch(), control.applied()))
                })
                .collect::<Result<Vec<_>, _>>()?
        ));
        Ok(())
    })
}

/// Every run gets its own ticket, and each repair applies exactly once, at a
/// tier drawn from the declared list.
///
/// The tier is the *first* entry of the list the seed selects, bounded to what a
/// seeded family can afford on every band: the full three-tier claim is the
/// explicit measurement below, and this family exists to sweep the property — a
/// ticket per run, one application each, no id collision — across many seeds
/// rather than to re-measure scale sixteen times.
pub(super) fn saturation_applies_each_ticket_once(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        // A small drawn tier, not one of the declared scale tiers. This family
        // sweeps the *property* — every run gets a ticket, each applies once, no
        // two collide — across sixteen bands and every seed twice over; at the
        // 100+ tiers each band costs minutes of real `fsync` and the suite stops
        // being a sweep. The declared 100 / 1,000 / 10,000 scale claim is the
        // tier measurement in `tiers.rs`, which prints the level it reached.
        let bound = saturating_levels();
        let tier = usize::try_from(sim.rng().between(4, bound))?;
        let dir = sim.scratch("repair-scale")?;
        let host = blocked_host(TENANTS[0], &dir)?;
        let declared = shared::journey_task()?;
        // The grant answers exactly the ticket each run mints, so a repair is
        // authorized here for the same reason it would be in production: the
        // grant covers the whole shortfall the run reported.
        let grants = exact_grant(candidate_needs().len());

        let mut tickets = Vec::with_capacity(tier);
        for _ in 0..tier {
            let polls = Polls::shared();
            let report = lgwks_bot::block_on(host.run(
                &declared,
                input_needing(Arc::clone(&polls), 7, candidate_needs()),
            ));
            assert_eq!(
                report.disposition(),
                Disposition::Blocked,
                "every run at this tier blocks: {:?}",
                report.error()
            );
            let ticket = report
                .repair()
                .ok_or("a blocked run carries a ticket")?
                .clone();
            let run = report.run_id().ok_or("a stored run names its run id")?;
            tickets.push(Case {
                ticket,
                run,
                declared: shared::journey_task()?,
                polls,
                host: host.clone(),
            });
        }
        assert_eq!(
            tickets.len(),
            tier,
            "tier {tier}: every run produced its own ticket"
        );
        let mut ids: Vec<String> = tickets
            .iter()
            .map(|entry| entry.run.id().to_hex())
            .collect();
        ids.sort_unstable();
        let distinct = ids.len();
        ids.dedup();
        assert_eq!(
            ids.len(),
            distinct,
            "tier {tier}: no two runs minted the same id"
        );

        // Every ticket answered exactly once, and every answer is a success: the
        // run each ticket names was blocked only by this run's own shortfall.
        for (index, entry) in tickets.iter().enumerate() {
            let &Case {
                ref ticket,
                run,
                ref polls,
                ..
            } = entry;
            let report = lgwks_bot::block_on(host.repair(
                ticket,
                &grants,
                &declared,
                input(Arc::clone(polls), 7),
                1,
            ))?;
            assert_eq!(
                report.disposition(),
                Disposition::Succeeded,
                "tier {tier}: repair {index} must close the run it names: {:?}",
                report.error()
            );
            assert_eq!(
                polls.publish(),
                1,
                "tier {tier}: repair {index} published once"
            );
            let control = shared::ledger_of(&host)?
                .control(run)
                .ok_or("the charged run has a control state")?;
            assert_eq!(
                (control.epoch(), control.applied()),
                converged(1),
                "tier {tier}: repair {index} applied its ticket exactly once"
            );
        }

        // One more delivery of every ticket: all refused, nothing re-applied.
        for (index, entry) in tickets.iter().enumerate() {
            match lgwks_bot::block_on(host.repair(
                &entry.ticket,
                &grants,
                &declared,
                input(Polls::shared(), 7),
                1,
            )) {
                Err(RepairError::AlreadyApplied) | Err(RepairError::StaleEpoch { .. }) => {}
                other => {
                    let refusal = Err(format!(
                        "tier {tier}: redelivery {index} must be refused, got {other:?}"
                    )
                    .into());
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "saturation_applies_each_ticket_once: returning an error to the caller");
                    return refusal;
                }
            }
        }
        sim.record(&format!(
            "tier-requested={tier} tier-reached={tier} runs={} admitted={} refused={}",
            tickets.len(),
            host.admission().admitted(),
            host.admission().refused()
        ));
        Ok(())
    })
}
