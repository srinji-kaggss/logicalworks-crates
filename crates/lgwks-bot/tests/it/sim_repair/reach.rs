//! The arms: where a reach blocks, what a wide need costs, reopen survival, custom capabilities, tenant isolation and the decision order.

use super::*;

// ── Arms ───────────────────────────────────────────────────────────────────

/// T23: a task that reaches in its **first** step is blocked at the admission
/// boundary, and the ticket names every need it declared.
///
/// The journey blocks *after* its analysis, which is the case a repair exists for;
/// this is the other half of the front door, and it has a different shape at every
/// step. Nothing is recorded (so there is nothing for a repair to replay and a
/// repair's grant is the whole of the run's authority), no body is polled, the run
/// is never admitted, the ledger is never charged — and no ticket minted for it
/// could be applied, because there is no budget to apply it against — while the
/// report still names the complete shortfall and the ticket names the same set.
#[test]
fn the_step_that_reaches_is_the_step_that_blocks() -> TestResult {
    // The declared need set is the whole of the candidate vocabulary at its widest:
    // a task declaring all four against a host granting none is the largest
    // complete shortfall the admission boundary has to name in one pass, which is
    // T23's "one complete presently knowable NeedSet" at its extreme. The sweep
    // over the narrower widths is the family below.
    let declared_needs = candidate_needs();
    let scratch = Scratch::new("first-step")?;
    let host = repairable_host(TENANTS[0], scratch.path(), GrantSet::empty())?;
    let polls = Polls::shared();
    let declared = first_step_task(declared_needs.clone())?;

    let report = lgwks_bot::block_on(host.run(
        &declared,
        input_needing(Arc::clone(&polls), 7, declared_needs.clone()),
    ));

    assert_eq!(
        report.disposition(),
        Disposition::Blocked,
        "a first-step reach is Blocked, not Failed: {:?}",
        report.error()
    );
    assert_eq!(
        names(shared::ticket_of(&report)?.needs()),
        names(&declared_needs),
        "the ticket names the complete declared shortfall, in the order it was declared"
    );
    assert_eq!(
        report.needs().map(Deficit::len),
        Some(declared_needs.len()),
        "the report's shortfall holds one entry per declared need: a caller granting what \
         they were told must not be refused once per capability to learn all four"
    );
    assert_eq!(
        polls.publish(),
        0,
        "the boundary refusal is before the body, so no step ran"
    );
    let run = shared::run_of(&report)?;
    assert_eq!(
        shared::store_of(&host)?.record_count(run),
        0,
        "a run refused before admission records nothing, so a repair would replay nothing \
         and the ticket's grant would be the whole of the run's authority"
    );
    assert_eq!(
        host.admission().admitted(),
        0,
        "the run never reached admission, so no permit was taken"
    );
    assert!(
        shared::ledger_of(&host)?.control(run).is_none(),
        "a run refused at the boundary is not charged a root attempt: there is no budget to \
         decide a later repair against, which is why the ticket is still answerable only \
         by a host that would have to create it"
    );
    Ok(())
}

/// T23: the analysis is recorded once however wide the shortfall is, and the
/// ticket's needs are exactly that shortfall.
///
/// The multi-capability end of T23's "one complete NeedSet", swept across the
/// whole candidate vocabulary rather than at one width: a four-capability
/// shortfall must cost the same one analysis as a one-capability one, because the
/// reach happens at the publication step and the analysis committed before it.
#[test]
fn a_wide_need_set_costs_one_analysis() -> TestResult {
    let whole = candidate_needs();
    for reach in 1..=whole.len() {
        let scratch = Scratch::new("wide-need")?;
        let host = repairable_host(TENANTS[0], scratch.path(), GrantSet::empty())?;
        let polls = Polls::shared();
        let declared = shared::journey_task()?;
        let needs: Vec<Cap> = whole.iter().take(reach).cloned().collect();

        let report = lgwks_bot::block_on(host.run(
            &declared,
            input_needing(Arc::clone(&polls), 7, needs.clone()),
        ));

        assert_eq!(
            report.disposition(),
            Disposition::Blocked,
            "reach {reach}: the run is blocked on what it is short of: {:?}",
            report.error()
        );
        assert_eq!(
            names(shared::ticket_of(&report)?.needs()),
            names(&needs),
            "reach {reach}: the ticket names the shortfall in the order the reach named it"
        );
        assert_eq!(
            names(
                &report
                    .needs()
                    .ok_or("a blocked run names its shortfall")?
                    .shortages()
                    .map(|shortage| shortage.required().clone())
                    .collect::<Vec<_>>()
            ),
            names(&needs),
            "reach {reach}: the report and the ticket come from one Deficit, so they \
             cannot disagree about what the run was missing"
        );
        assert_eq!(
            polls.analysis(),
            1,
            "reach {reach}: {reach} missing capabilities cost one analysis, not {reach}"
        );
        assert_eq!(
            shared::store_of(&host)?.record_count(shared::run_of(&report)?),
            1,
            "reach {reach}: the one record is the analysis, and it is the whole of what a \
             repair replays"
        );
    }
    Ok(())
}

/// T23: the repaired run's replay rests on the bytes on the disk, not on a handle.
///
/// The repair is delivered to a **second** host built over the same directory. If
/// the replay were served from anything the first handle kept, this would fail;
/// what it asserts instead is the claim INV-BOT-54 makes — the analysis was read
/// back from the record store's own file, so its body was not polled a second
/// time and the publication ran exactly once, on the run that asked for it.
#[test]
fn a_repaired_run_survives_a_reopened_host() -> TestResult {
    let scratch = Scratch::new("reopened-repair")?;
    let first = repairable_host(TENANTS[0], scratch.path(), GrantSet::empty())?;
    let polls = Polls::shared();
    let declared = shared::journey_task()?;

    let blocked = lgwks_bot::block_on(first.run(
        &declared,
        input_needing(Arc::clone(&polls), 7, shared::default_needs()),
    ));
    let ticket = shared::ticket_of(&blocked)?.clone();
    let run = shared::run_of(&blocked)?;
    assert_eq!(polls.analysis(), 1, "the first host recorded the analysis");

    // Drop the first host entirely: its handles, its ledger and its store. What is
    // left is the directory.
    drop(first);

    let second = repairable_host(TENANTS[0], scratch.path(), GrantSet::empty())?;
    let after = charged(&second, run)?;
    assert_eq!(
        (after.attempts(), after.epoch(), after.applied()),
        (1, 0, 0),
        "a reopened ledger replays the charged budget and the epoch off the file, so the \
         ticket is still answerable by a host that was never running"
    );

    let repaired = lgwks_bot::block_on(second.repair(
        &ticket,
        &GrantSet::empty().grant(Cap::new(Cap::NET)),
        &declared,
        input(Arc::clone(&polls), 7),
        1,
    ))?;
    assert_eq!(
        repaired.disposition(),
        Disposition::Succeeded,
        "the second host closed the run: {:?}",
        repaired.error()
    );
    assert_eq!(repaired.run_id(), Some(run));
    assert_eq!(
        polls.analysis(),
        1,
        "the analysis body was not polled again: its value came off the disk"
    );
    assert_eq!(
        polls.publish(),
        1,
        "the blocked remainder ran exactly once, on the host that answered the ticket"
    );
    Ok(())
}

/// T24: the over-wide check is width-independent, and it sees a name the ticket
/// never asked for even when it is the only one the grant adds.
///
/// The defect this pins once stood: the check asked about a fixed list of the four
/// shipped capabilities, so a grant carrying one of them *plus* a custom name
/// passed and the repair applied the whole grant. One need and four are the two
/// widths at which a candidate-driven check and a whole-grant check disagree, and
/// both must refuse.
#[test]
fn a_custom_capability_is_refused_at_every_width() -> TestResult {
    let whole = candidate_needs();
    for reach in 1..=whole.len() {
        let scratch = Scratch::new("custom-cap")?;
        let host = repairable_host(TENANTS[0], scratch.path(), GrantSet::empty())?;
        let case = block(TENANTS[0], scratch.path(), reach)?;
        let bytes = ledger_bytes(&host)?;

        let smuggled = exact_grant(reach).grant(Cap::new("vendor.payments.charge"));
        match lgwks_bot::block_on(case.host.repair(
            &case.ticket,
            &smuggled,
            &case.declared,
            input(Arc::clone(&case.polls), 7),
            1,
        )) {
            Err(RepairError::OverWide { ref beyond }) => assert_eq!(
                names(beyond),
                vec!["vendor.payments.charge"],
                "reach {reach}: the refusal names the one capability the ticket never asked for"
            ),
            other => {
                return Err(format!(
                    "reach {reach}: a custom capability outside the ticket must be refused: \
                     got {other:?}"
                )
                .into());
            }
        }
        assert_eq!(
            case.polls.publish(),
            0,
            "reach {reach}: a refused repair runs no step"
        );
        assert_eq!(
            charged(&case.host, case.run)?.epoch(),
            0,
            "reach {reach}: a refused repair mints no epoch"
        );
        assert_eq!(
            ledger_bytes(&case.host)?,
            bytes,
            "reach {reach}: the refused repair left the ledger byte-identical"
        );
        assert!(
            host.admission().refused() == 0,
            "reach {reach}: the refusal happened before admission, so the host saw no run"
        );
    }
    Ok(())
}

/// T24: a ticket never reaches another tenant's host, and its refusal leaves no
/// trace in the ledger the asking host can read.
///
/// Two hosts of two tenants over one directory, which is the only arrangement in
/// which a cross-tenant repair is even expressible. The refusal has to be the
/// *ticket's* tenant check rather than the ledger's, and that is observable rather
/// than asserted: the ledger names `RepairError::ForeignTenant` and the other
/// tenant's run is not in it.
#[test]
fn a_ticket_never_names_another_tenants_run() -> TestResult {
    let scratch = Scratch::new("cross-tenant-ticket")?;
    let mine = repairable_host(TENANTS[0], scratch.path(), GrantSet::empty())?;
    let theirs = repairable_host(TENANTS[1], scratch.path(), GrantSet::empty())?;
    let declared = shared::journey_task()?;

    // The wide shortfall: a ticket naming every candidate, so the refusal cannot be
    // a grant check that happened to pass.
    let report = lgwks_bot::block_on(mine.run(
        &declared,
        input_needing(Polls::shared(), 7, candidate_needs()),
    ));
    let ticket = shared::ticket_of(&report)?.clone();
    let run = shared::run_of(&report)?;

    match lgwks_bot::block_on(theirs.repair(
        &ticket,
        &exact_grant(candidate_needs().len()),
        &declared,
        input(Polls::shared(), 7),
        1,
    )) {
        Err(RepairError::ForeignTenant {
            ref ticket,
            ref host,
        }) => assert_eq!(
            (ticket.as_str(), host.as_str()),
            (TENANTS[0], TENANTS[1]),
            "the refusal names the ticket's tenant and the host's, so a reader can see which \
             is which"
        ),
        other => {
            return Err(format!("another tenant's ticket must be refused: got {other:?}").into());
        }
    }
    assert_eq!(
        charged(&mine, run)?.epoch(),
        0,
        "the refused repair minted no epoch on the tenant that owns the run"
    );
    assert!(
        theirs
            .run_ledger()
            .is_some_and(|ledger| ledger.control(run).is_none()),
        "the asking tenant's ledger has no entry for a run it does not own: the run \
         belongs to the other tenant's ledger file, and it is never created here"
    );
    assert_eq!(
        theirs.admission().admitted(),
        0,
        "the refusal was before admission, so the asking host never took a permit for it"
    );
    Ok(())
}

/// T24: each arm of the repair door, asserted against the draws that reach it.
///
/// The order family sweeps the *endpoint* however the decisions were sequenced;
/// this sweeps the *witnesses*, so a caller can see which arm each refusal is and
/// that a denial charges nothing. It is the one seeded family that pins the typed
/// arm rather than the counts, which is the gap the endpoint-only assertion leaves:
/// two arms that both left the run where it was would both pass there.
#[test]
fn a_mixed_decision_order_pins_each_arm() -> TestResult {
    sim::assert_replays(Band::new(96, 8), |sim| {
        let plan = plan(sim.rng())?;
        let dir = sim.scratch("repair-arms")?;
        let case = block(TENANTS[0], &dir, plan.reach)?;
        let Case {
            ref host,
            ref ticket,
            run,
            ref declared,
            ref polls,
        } = case;

        for (index, action) in plan.actions.iter().enumerate() {
            let before = charged(host, run)?;
            let grant = match *action {
                Action::Repair | Action::Duplicate => exact_grant(plan.reach),
                Action::Deny => short_grant(plan.reach),
                Action::OverWide => wide_grant(plan.reach),
                Action::Inspect => continue,
            };
            let outcome = lgwks_bot::block_on(host.repair(
                ticket,
                &grant,
                declared,
                input(Arc::clone(polls), 7),
                1,
            ));
            let arm = arm_of(&outcome);
            let after = charged(host, run)?;

            match *action {
                Action::Repair | Action::Duplicate => assert!(
                    matches!(arm, "succeeded" | "already-applied" | "stale-epoch"),
                    "decision {index}: an exact grant applies the ticket or is refused for \
                     having applied it already, never for its content; got {arm}"
                ),
                Action::Deny => {
                    assert!(
                        matches!(arm, "not-authorized" | "over-wide"),
                        "decision {index}: a short grant is a denial; at a one-capability \
                         ticket the over-wide check fires first, so one width reports one \
                         arm and the wider ones the other. Got {arm}"
                    );
                    assert_eq!(
                        (after.attempts(), after.spend(), after.epoch()),
                        (before.attempts(), before.spend(), before.epoch()),
                        "decision {index}: a denial charges nothing and mints no epoch"
                    );
                }
                Action::OverWide => {
                    assert_eq!(
                        arm, "over-wide",
                        "decision {index}: a wide grant is refused"
                    );
                    assert_eq!(
                        (after.attempts(), after.spend(), after.epoch()),
                        (before.attempts(), before.spend(), before.epoch()),
                        "decision {index}: an over-wide grant charges nothing and mints no epoch"
                    );
                }
                Action::Inspect => {}
            }
            if matches!(arm, "succeeded" | "already-applied" | "stale-epoch") {
                assert!(
                    after.epoch() <= 1,
                    "decision {index}: one ticket applies at most once, whatever the order; \
                     the run is at epoch {}",
                    after.epoch()
                );
            }
            sim.record(&format!("decision {index} {} arm={arm}", action.label()));
        }
        let end = charged(host, run)?;
        assert_eq!(
            end.epoch(),
            u64::try_from(end.applied())?,
            "the epoch and the applied count are one fact: one at each after a repair, zero \
             at both without one"
        );
        sim.record(&format!(
            "ordinal={} reach={} decisions={} epoch={} applied={} attempts={}",
            plan.ordinal,
            plan.reach,
            plan.actions.len(),
            end.epoch(),
            end.applied(),
            end.attempts()
        ));
        Ok(())
    })
}
