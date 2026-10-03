//! Repairing a blocked run, through the public front door.
//!
//! | Test | What it pins |
//! |---|---|
//! | `a_run_short_of_authority_is_blocked_naming_every_need` | T23: four missing capabilities come back as one complete shortfall, with a ticket naming exactly them, and nothing after the blocked step ran |
//! | `a_repair_runs_the_blocked_remainder_without_rerunning_the_analysis` | T23: the analysis body is polled once across both attempts; the publication polls once, after the repair |
//! | `a_repair_widens_one_run_and_not_the_host` | T23: the repair's authority is this run's, and the host's own grant is unchanged afterwards |
//! | `the_same_ticket_delivered_twice_applies_once` | T24: the second delivery is refused and nothing repeats |
//! | `a_ticket_from_an_older_epoch_is_refused_as_stale` | T24: an old ticket cannot re-apply authority the run has moved past |
//! | `a_denied_repair_leaves_the_run_blocked_with_its_authority_unchanged` | T24: a grant that does not cover the ticket's needs runs no step, charges no budget and moves no epoch |
//! | `an_over_wide_grant_is_refused_rather_than_narrowed` | T24: a grant reaching past the ticket is refused, and names what it reached for |
//! | `a_host_without_a_ledger_refuses_every_repair` | T24: nothing to decide a repair against is a refusal, not an unchecked application |
//! | `the_root_budget_stays_charged_across_repair_and_resume` | T13: a repair consumes the root budget rather than resetting it, and a spent budget refuses at a finite attempt |
//! | `a_denied_repair_costs_nothing` | T13/T24: no budget, no epoch, no step — the run's ledger is byte-identical |
//! | `a_blocked_run_leaves_its_finished_analysis_recorded` | T23: the store holds the analysis, so the repair's replay rests on bytes rather than on the counter alone |
//!
//! Every test drives the real `Host`, the real `Task`, the real step store and the
//! real repair ledger. Nothing here reimplements admission, a repair decision or a
//! budget; the counters are the only test-side instrument, and they exist because
//! "the analysis was not rerun" is not observable from a return value.

#![cfg(all(feature = "script", feature = "ephemeral"))]

use std::error::Error;
use std::sync::Arc;

use lgwks_bot::cap::Cap;
use lgwks_bot::effect::RunId;
use lgwks_bot::gate::GrantSet;
use lgwks_bot::task::{Disposition, Host, RepairError, RepairTicket, Report};

#[path = "support/repair.rs"]
mod shared;

use shared::{
    Journey, Polls, Scratch, TestResult, candidates, input, input_needing, repairable_host, run_of,
    store_of, ticket_of, unrepairable_host,
};

/// Run one attempt of the journey on `host`.
fn attempt(
    host: &Host,
    polls: &Arc<Polls>,
    needs: Vec<Cap>,
) -> Result<Report<u32>, Box<dyn Error>> {
    let declared = shared::journey_task()?;
    Ok(lgwks_bot::block_on(host.run(
        &declared,
        input_needing(Arc::clone(polls), 7, needs),
    )))
}

/// Answer `ticket` with `grant`, charging `spend` against the run's root budget.
fn repair(
    host: &Host,
    declared: &Journey,
    ticket: &RepairTicket,
    grant: GrantSet,
    spend: u64,
    polls: &Arc<Polls>,
) -> Result<Report<u32>, RepairError> {
    lgwks_bot::block_on(host.repair(
        ticket,
        &grant,
        declared,
        input(Arc::clone(polls), 7),
        spend,
        &candidates(),
    ))
}

/// A blocked run that reached for `needs`, with everything a caller needs to
/// answer it: the host, the task, the ticket, the run and the shared counters.
///
/// One fixture rather than the preamble repeated per test, because the preamble
/// *is* the case — a host that grants nothing, one journey attempt, and the two
/// facts read off its report. A copy per test is how one test's setup drifts from
/// another's, and a test that forgot a piece would fail for a reason its own name
/// never says.
struct Blocked {
    /// The scratch directory, held so it outlives every read against it.
    scratch: Scratch,
    /// The host the run was attempted on.
    host: Host,
    /// The task the run used.
    declared: Journey,
    /// The counters both attempts of this run share.
    polls: Arc<Polls>,
    /// The ticket the blocked report handed back.
    ticket: RepairTicket,
    /// The run that is blocked.
    run: RunId,
    /// The report itself, for the tests that read more than the ticket off it.
    report: Report<u32>,
}

/// Drive one blocked attempt reaching for `needs` on a fresh scratch directory.
fn blocked(tag: &'static str, needs: Vec<Cap>) -> Result<Blocked, Box<dyn Error>> {
    let scratch = Scratch::new(tag)?;
    let host = repairable_host("acme", scratch.path(), GrantSet::empty())?;
    let declared = shared::journey_task()?;
    let polls = Polls::shared();
    let report = attempt(&host, &polls, needs)?;
    Ok(Blocked {
        ticket: ticket_of(&report)?.clone(),
        run: run_of(&report)?,
        host,
        declared,
        polls,
        scratch,
        report,
    })
}

/// The four capabilities a run is short of when the host grants none.
fn all_four() -> Vec<Cap> {
    vec![
        Cap::new(Cap::NET),
        Cap::new(Cap::FS),
        Cap::new(Cap::SYS),
        Cap::new(Cap::NOTIFY),
    ]
}

/// The four names, for comparing against a report's shortfall of `&str`s.
const ALL_FOUR_NAMES: [&str; 4] = [Cap::NET, Cap::FS, Cap::SYS, Cap::NOTIFY];

/// T23: several missing capabilities come back as one complete shortfall, with a
/// ticket naming exactly them, and nothing after the blocked step ran.
#[test]
fn a_run_short_of_authority_is_blocked_naming_every_need() -> TestResult {
    let case = blocked("needs", all_four())?;

    assert_eq!(
        case.report.disposition(),
        Disposition::Blocked,
        "a run short of authority is Blocked, not Failed: {:?}",
        case.report.error()
    );
    let shortfall = case
        .report
        .needs()
        .ok_or("a blocked run names its shortfall")?;
    let named: Vec<&str> = shortfall
        .shortages()
        .map(|shortage| shortage.required().as_str())
        .collect();
    assert_eq!(
        named, ALL_FOUR_NAMES,
        "one report must carry the whole shortfall: a caller granting what they \
         were told must not have to be refused once per capability to learn all four"
    );

    // The ticket is built from the same shortfall, so it names the same set.
    let wanted: Vec<&str> = case.ticket.needs().iter().map(Cap::as_str).collect();
    assert_eq!(
        wanted, ALL_FOUR_NAMES,
        "the ticket names exactly what the report says was missing"
    );
    assert_eq!(
        case.ticket.tenant(),
        "acme",
        "the ticket names the tenant that owns the run"
    );
    assert_eq!(
        case.ticket.epoch(),
        0,
        "a run no repair has touched is at epoch zero"
    );
    assert_eq!(
        case.ticket.run(),
        case.run,
        "the ticket names the run that is blocked"
    );

    // The analysis reached and recorded; the publication did not.
    assert_eq!(
        case.polls.analysis(),
        1,
        "the analysis ran once before the block"
    );
    assert_eq!(
        case.polls.publish(),
        0,
        "the blocked step's body never ran, so no publication happened"
    );
    assert_eq!(
        case.report.ended_at(),
        Some("publish/publish"),
        "the report names the step that reached for the authority"
    );
    Ok(())
}

/// T23: a repair runs the blocked remainder and replays the recorded analysis.
#[test]
fn a_repair_runs_the_blocked_remainder_without_rerunning_the_analysis() -> TestResult {
    let case = blocked("repair", shared::default_needs())?;
    assert_eq!(case.polls.analysis(), 1);
    assert_eq!(
        case.polls.publish(),
        0,
        "the publication is blocked, not done"
    );

    let repaired = repair(
        &case.host,
        &case.declared,
        &case.ticket,
        GrantSet::empty().grant(Cap::new(Cap::NET)),
        1,
        &case.polls,
    )?;

    assert_eq!(
        repaired.disposition(),
        Disposition::Succeeded,
        "the repaired run reaches its output: {:?}",
        repaired.error()
    );
    assert_eq!(repaired.output(), Some(&7));
    assert_eq!(
        repaired.run_id(),
        Some(case.run),
        "a repair resumes the run it was given; it is not a second run"
    );
    assert_eq!(
        case.polls.analysis(),
        1,
        "the recorded analysis must not be polled again: a repair that re-ran it \
         would be paying twice for work the store already holds"
    );
    assert_eq!(
        case.polls.publish(),
        1,
        "the blocked remainder runs exactly once, after the repair"
    );
    Ok(())
}

/// T23: the repair authorizes one run, and the host's own grant is unchanged.
#[test]
fn a_repair_widens_one_run_and_not_the_host() -> TestResult {
    let case = blocked("scope", shared::default_needs())?;

    repair(
        &case.host,
        &case.declared,
        &case.ticket,
        GrantSet::empty().grant(Cap::new(Cap::NET)),
        1,
        &case.polls,
    )?;

    assert!(
        !case.host.grants().grants(&Cap::new(Cap::NET)),
        "the host's grant is the host's own authority; a repair authorizes one run \
         and must not widen the host for the next one"
    );
    let after = attempt(&case.host, &Polls::shared(), shared::default_needs())?;
    assert_eq!(
        after.disposition(),
        Disposition::Blocked,
        "the next run on this host is still blocked, so the repair was scoped to \
         the run that asked for it"
    );
    Ok(())
}

/// A blocked run whose ticket has already been answered once, so a second
/// delivery is the case under test.
///
/// The fixture for the two replay-shaped rows — the same ticket twice, and a
/// ticket from an epoch the run has moved past — because both are the same
/// precondition reached by a different route: one repair applied, and a second
/// attempt to apply the ticket that was already used.
struct Answered {
    /// The blocked run's own state, kept alive alongside the repair below.
    blocked: Blocked,
    /// The grant that answers the ticket exactly.
    grant: GrantSet,
    /// The first repair's report.
    first: Report<u32>,
}

/// Answer `case`'s ticket once, with the grant covering exactly `needs`.
fn answered(case: Blocked, needs: &[Cap]) -> Result<Answered, Box<dyn Error>> {
    let grant = needs
        .iter()
        .fold(GrantSet::empty(), |held, cap| held.grant(cap.clone()));
    let first = repair(
        &case.host,
        &case.declared,
        &case.ticket,
        grant.clone(),
        1,
        &case.polls,
    )?;
    Ok(Answered {
        blocked: case,
        grant,
        first,
    })
}

/// T24: the same ticket delivered twice applies once.
#[test]
fn the_same_ticket_delivered_twice_applies_once() -> TestResult {
    let answered_case = answered(
        blocked("dupe", shared::default_needs())?,
        &[Cap::new(Cap::NET)],
    )?;
    let case = &answered_case.blocked;
    assert_eq!(answered_case.first.disposition(), Disposition::Succeeded);
    assert_eq!(case.polls.publish(), 1, "the publication ran once");

    match repair(
        &case.host,
        &case.declared,
        &case.ticket,
        answered_case.grant.clone(),
        1,
        &case.polls,
    ) {
        Err(RepairError::AlreadyApplied) | Err(RepairError::StaleEpoch { .. }) => {}
        other => {
            return Err(format!("a ticket delivered twice must be refused: got {other:?}").into());
        }
    }
    assert_eq!(
        case.polls.publish(),
        1,
        "the second delivery must not repeat an effect the first already applied"
    );

    let control = shared::ledger_of(&case.host)?
        .control(case.run)
        .ok_or("a charged run has a control state")?;
    assert_eq!(
        control.epoch(),
        1,
        "applying a ticket advances the epoch once, so the redelivery is both a \
         duplicate and stale"
    );
    assert_eq!(
        control.applied(),
        1,
        "the ticket is recorded applied once however many times it is delivered"
    );
    Ok(())
}

/// T24: a ticket from an older epoch cannot re-apply authority.
#[test]
fn a_ticket_from_an_older_epoch_is_refused_as_stale() -> TestResult {
    let answered_case = answered(
        blocked("stale", shared::default_needs())?,
        &[Cap::new(Cap::NET)],
    )?;
    let case = &answered_case.blocked;
    assert_eq!(
        case.ticket.epoch(),
        0,
        "the first ticket is minted at epoch zero"
    );
    assert_eq!(answered_case.first.disposition(), Disposition::Succeeded);

    // The same ticket, delivered again after the epoch moved on. A caller that
    // rebuilt it from the facts it knew before the first repair carries the old
    // epoch, and that is exactly the case this refuses.
    match repair(
        &case.host,
        &case.declared,
        &case.ticket,
        answered_case.grant.clone(),
        1,
        &case.polls,
    ) {
        Err(RepairError::StaleEpoch { current, offered }) => {
            assert_eq!(
                (current, offered),
                (1, 0),
                "the refusal names both epochs, so a reader can see which is which"
            );
        }
        other => return Err(format!("an old ticket must be refused: got {other:?}").into()),
    }
    Ok(())
}
/// T24: a repair whose grant does not cover the ticket leaves the run untouched.
#[test]
fn a_denied_repair_leaves_the_run_blocked_with_its_authority_unchanged() -> TestResult {
    let case = blocked("denied", all_four())?;

    // A grant covering one of the four: the repair would leave the run blocked, so
    // it is a denial rather than a repair.
    match repair(
        &case.host,
        &case.declared,
        &case.ticket,
        GrantSet::empty().grant(Cap::new(Cap::NET)),
        1,
        &case.polls,
    ) {
        Err(RepairError::NotAuthorized { ref missing }) => {
            let names: Vec<&str> = missing.iter().map(Cap::as_str).collect();
            assert_eq!(
                names,
                vec![Cap::FS, Cap::SYS, Cap::NOTIFY],
                "the refusal names what the grant is missing, in the ticket's order"
            );
        }
        other => return Err(format!("a partial grant must be refused: got {other:?}").into()),
    }
    assert_eq!(case.polls.publish(), 0, "a denied repair runs no step");
    assert!(
        !case.host.grants().grants(&Cap::new(Cap::NET)),
        "a denied repair leaves the host's authority exactly as it was"
    );
    assert_eq!(
        shared::ledger_of(&case.host)?
            .control(case.run)
            .map_or(0, |control| control.epoch()),
        0,
        "a denied repair mints no epoch, so the run is still where it was"
    );
    Ok(())
}

/// T24: a grant reaching past the ticket is refused and names what it reached for.
#[test]
fn an_over_wide_grant_is_refused_rather_than_narrowed() -> TestResult {
    let case = blocked("wide", vec![Cap::new(Cap::NET)])?;
    let wide = GrantSet::empty()
        .grant(Cap::new(Cap::NET))
        .grant(Cap::new(Cap::SYS));

    match repair(
        &case.host,
        &case.declared,
        &case.ticket,
        wide,
        1,
        &case.polls,
    ) {
        Err(RepairError::OverWide { ref beyond }) => {
            assert_eq!(
                beyond,
                &vec![Cap::new(Cap::SYS)],
                "the refusal names exactly the capability the ticket never asked for"
            );
        }
        other => return Err(format!("a wider grant must be refused: got {other:?}").into()),
    }
    assert_eq!(case.polls.publish(), 0, "a refused repair runs no step");
    assert_eq!(
        shared::ledger_of(&case.host)?
            .control(case.run)
            .map_or(0, |control| control.epoch()),
        0,
        "a refused repair mints no epoch"
    );
    Ok(())
}

/// T24: a host with nothing to decide a repair against refuses every repair.
#[test]
fn a_host_without_a_ledger_refuses_every_repair() -> TestResult {
    let scratch = Scratch::new("noledger")?;
    // A run store but no ledger: the analysis is replayable and the run is
    // blocked, and there is no epoch, budget or applied-ticket set to decide a
    // repair against.
    let host = unrepairable_host("acme", scratch.path(), GrantSet::empty())?;
    let polls = Polls::shared();

    let blocked = attempt(&host, &polls, shared::default_needs())?;
    assert_eq!(blocked.disposition(), Disposition::Blocked);
    assert!(
        blocked.repair().is_none(),
        "a host with no ledger names its needs but mints no ticket: a ticket whose \
         budget and epoch nothing could be checked against would be an authority a \
         caller could apply unchecked"
    );
    assert!(
        host.run_ledger().is_none(),
        "this host is the one without a ledger, which is what the case is"
    );
    assert_eq!(
        blocked.needs().map(|deficit| deficit.len()),
        Some(1),
        "the shortfall is still reported: what is missing is repairable, not known"
    );
    Ok(())
}

/// T13: the root budget is charged by a repair and never refilled by one.
#[test]
fn the_root_budget_stays_charged_across_repair_and_resume() -> TestResult {
    let scratch = Scratch::new("budget")?;
    // Three attempts is the whole budget: one blocked run, one repair, and one
    // spare, so a fourth attempt is the one the budget refuses.
    let host = Host::builder("acme")?
        .grants(GrantSet::empty())
        .run_store(scratch.path())?
        .repair_ledger(scratch.path())?
        .repair_bounds(3, 64)
        .build()?;
    let polls = Polls::shared();
    let declared = shared::journey_task()?;

    let blocked = attempt(&host, &polls, shared::default_needs())?;
    let ticket = ticket_of(&blocked)?.clone();
    let run = run_of(&blocked)?;
    let ledger = shared::ledger_of(&host)?;
    let charged = ledger
        .control(run)
        .ok_or("the blocked attempt charged its run")?;
    assert_eq!(
        charged.attempts(),
        1,
        "the blocked attempt charged one root attempt"
    );

    let repaired = repair(
        &host,
        &declared,
        &ticket,
        GrantSet::empty().grant(Cap::new(Cap::NET)),
        1,
        &polls,
    )?;
    assert_eq!(repaired.disposition(), Disposition::Succeeded);
    let after = ledger
        .control(run)
        .ok_or("the repair charged the same run")?;
    assert_eq!(
        after.attempts(),
        2,
        "a repair is an attempt and consumes the root budget; it does not reset it"
    );
    assert_eq!(
        after.spend(),
        charged.spend().saturating_add(1),
        "the repair's own spend is charged on top of what the run had already spent"
    );

    // The next resume of the same run is the budget's last attempt, and the one
    // after it is refused — a finite refusal, not an unbounded retry loop.
    let again = lgwks_bot::block_on(host.resume(run, &declared, input(polls, 7)));
    assert!(
        again.disposition().was_admitted(),
        "a resume within the budget still runs: {:?}",
        again.error()
    );
    assert_eq!(
        ledger
            .control(run)
            .ok_or("the resume charged the same run")?
            .attempts(),
        3,
        "the run is now at its budget"
    );
    let refused = lgwks_bot::block_on(host.resume(run, &declared, input(Polls::shared(), 7)));
    assert_eq!(
        refused.disposition(),
        Disposition::Refused,
        "a run whose root budget is spent is refused, so repeated NotApplied \
         reaches a finite answer rather than retrying forever"
    );
    assert!(
        refused
            .error()
            .is_some_and(|error| error.to_string().contains("root budget")),
        "the refusal says the budget is what refused it: {:?}",
        refused.error()
    );
    Ok(())
}

/// T13/T24: a refused repair costs the run nothing at all.
///
/// The ticket names all four capabilities, so each grant below is *short* rather
/// than over-wide: a caller narrowing by accident, asking for less authority than
/// the ticket says the run needs, is a denial too and must cost nothing.
#[test]
fn a_denied_repair_costs_nothing() -> TestResult {
    let case = blocked("nothing", all_four())?;
    let ledger = shared::ledger_of(&case.host)?;
    let before = ledger
        .control(case.run)
        .ok_or("the blocked attempt charged its run")?;
    let bytes_before = case.scratch.join("acme.runledger").metadata()?.len();

    for (grant, what) in [
        (GrantSet::empty(), "no authority at all"),
        (
            GrantSet::empty().grant(Cap::new(Cap::NET)),
            "a grant covering one of four",
        ),
        (
            GrantSet::empty()
                .grant(Cap::new(Cap::NET))
                .grant(Cap::new(Cap::FS)),
            "a grant covering two of four",
        ),
    ] {
        match repair(
            &case.host,
            &case.declared,
            &case.ticket,
            grant,
            1,
            &case.polls,
        ) {
            Err(RepairError::NotAuthorized { .. }) | Err(RepairError::OverWide { .. }) => {}
            other => return Err(format!("{what} must be refused: got {other:?}").into()),
        }
        let after = ledger
            .control(case.run)
            .ok_or("the run still has its control state")?;
        assert_eq!(
            (after.attempts(), after.spend(), after.epoch()),
            (before.attempts(), before.spend(), before.epoch()),
            "{what} must charge nothing and mint no epoch"
        );
        assert_eq!(
            case.scratch.join("acme.runledger").metadata()?.len(),
            bytes_before,
            "{what} must leave the ledger byte-identical"
        );
    }
    Ok(())
}

/// The store holds the analysis, so the repair's replay rests on bytes rather than
/// on the counter alone.
#[test]
fn a_blocked_run_leaves_its_finished_analysis_recorded() -> TestResult {
    let case = blocked("records", shared::default_needs())?;
    assert_eq!(
        store_of(&case.host)?.record_count(case.run),
        1,
        "the analysis committed before the block, which is what makes the repair a \
         replay rather than a rerun"
    );
    Ok(())
}

/// The journey's own shape, checked through the public surface: the task declares
/// nothing at its admission boundary, because the reach happens at the step that
/// reaches.
#[test]
fn the_journey_declares_no_admission_boundary_needs() -> TestResult {
    let declared: Journey = shared::journey_task()?;
    assert!(
        declared.needs().is_empty(),
        "a task that reaches at a later step must not be refused before it has \
         done the work before that step"
    );
    assert_eq!(
        input(Polls::shared(), 7).needs().to_vec(),
        vec![Cap::new(Cap::NET)],
        "the reach is a parameter of the step, not of the task"
    );
    Ok(())
}
