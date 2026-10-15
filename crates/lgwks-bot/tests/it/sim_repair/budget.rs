//! The root budget: read back on reopen, charged once per repair, refusing once spent, and per run rather than per host.

use super::*;

/// T13: a fresh handle over the same file reports the charged budget back, run
/// after run, whatever each run spent.
///
/// The counters are stored as absolute state in each ledger entry and folded on
/// replay, so the fact under test is that the replay and the live write agree: a
/// caller that reopened its ledger must not find a run at zero, and must not find
/// it at a *different* number than it left. The seed decides how many runs the one
/// ledger serves and how much each is charged, so the chain is walked rather than
/// sampled at one point — a fold that dropped the last entry would pass a
/// one-run check and fail the second.
#[test]
fn a_reopen_reads_back_the_charged_budget() -> TestResult {
    sim::assert_replays(Band::new(112, 8), |sim| {
        let dir = sim.scratch("repair-replay")?;
        let host = blocked_host(TENANTS[0], &dir)?;
        let runs = usize::try_from(sim.rng().between(2, 6))?;
        let mut live = Vec::with_capacity(runs);

        for _ in 0..runs {
            let polls = Polls::shared();
            let case = block(TENANTS[0], &dir, 1)?;
            let spend = u64::from(sim.rng().below(4));
            let outcome = lgwks_bot::block_on(case.host.repair(
                &case.ticket,
                &exact_grant(1),
                &case.declared,
                input(Arc::clone(&polls), 7),
                spend,
            ));
            if arm_of(&outcome) != "succeeded" {
                return Err(format!(
                    "a one-capability ticket answered exactly must succeed, \
                     got {}",
                    arm_of(&outcome)
                )
                .into());
            }
            let control = charged(&case.host, case.run)?;
            assert_eq!(
                (control.epoch(), control.applied()),
                converged(1),
                "one repair is at epoch one with one applied ticket, on the live reader"
            );
            live.push((case.run, control));
        }

        let path = shared::ledger_of(&host)?.path().to_path_buf();
        let reopened = RunLedger::open(&path)?;
        for (index, entry) in live.iter().enumerate() {
            let (run, expected) = (entry.0, entry.1.clone());
            let read_back = reopened
                .control(run)
                .ok_or("the replayed ledger knows every run the live one charged")?;
            assert_eq!(
                (read_back.tenant(), read_back.attempts(), read_back.spend()),
                (expected.tenant(), expected.attempts(), expected.spend()),
                "run {index}: a reopened ledger reports the same tenant, attempts and spend \
                 the live one does"
            );
            assert_eq!(
                (read_back.epoch(), read_back.applied()),
                (expected.epoch(), expected.applied()),
                "run {index}: the epoch and the applied-ticket count replay exactly, so which \
                 handle a caller read cannot decide what the run holds"
            );
        }
        // The band draws two to six runs, so an empty list is a defect to report,
        // never a spend of zero to record.
        let first = live.first().ok_or("the band charged at least one run")?;
        let last = live.last().ok_or("the band charged at least one run")?;
        sim.record(&format!(
            "runs={runs} replayed={} first-spend={} last-spend={}",
            live.len(),
            first.1.spend(),
            last.1.spend()
        ));
        Ok(())
    })
}

/// T13: every authorized repair charges the root budget exactly once, and every
/// refusal charges nothing.
///
/// The sequence is asserted rather than a total, because the total is exactly what
/// a reset would leave unchanged — a repair that *refilled* the budget would hold
/// the same final number on a run with one blocked attempt and one repair, and
/// only the per-delivery step tells the two apart. The refusal half is here rather
/// than in the order family because it is the same measurement against the other
/// side of the door: a refusal that charged one attempt instead of none would pass
/// every endpoint assertion and fail here.
#[test]
fn every_repair_charges_the_root_budget_once() -> TestResult {
    sim::assert_replays(Band::new(104, 8), |sim| {
        let plan = plan(sim.rng())?;
        let dir = sim.scratch("repair-charge")?;
        let case = block(TENANTS[0], &dir, plan.reach)?;
        let Case {
            ref host,
            ref ticket,
            run,
            ref declared,
            ..
        } = case;

        // The seed decides how many times the ticket is delivered and how much
        // each delivery is charged, so the sequence is walked under draws rather
        // than read off a hand-written list.
        let deliveries = usize::try_from(sim.rng().between(3, 9))?;
        let mut applied = 0_usize;
        let mut refused = 0_usize;
        for index in 0..deliveries {
            let before = charged(host, run)?;
            let spend = u64::from(sim.rng().below(4));
            let outcome = lgwks_bot::block_on(host.repair(
                ticket,
                &exact_grant(plan.reach),
                declared,
                input(Polls::shared(), 7),
                spend,
            ));
            let arm = arm_of(&outcome);
            let after = charged(host, run)?;

            if arm == "succeeded" {
                applied = applied.saturating_add(1);
                assert_eq!(
                    after.attempts(),
                    before.attempts().saturating_add(1),
                    "delivery {index}: a repair is an attempt, charged on top of what the run \
                     had already spent and never as a reset of it"
                );
                assert_eq!(
                    after.spend(),
                    before.spend().saturating_add(spend),
                    "delivery {index}: the repair's own spend lands on the run's root spend"
                );
                assert_eq!(
                    after.epoch(),
                    before.epoch().saturating_add(1),
                    "delivery {index}: applying a ticket advances the epoch exactly once"
                );
            } else {
                refused = refused.saturating_add(1);
                assert!(
                    matches!(
                        arm,
                        "already-applied"
                            | "stale-epoch"
                            | "not-authorized"
                            | "over-wide"
                            | "budget-spent"
                            | "refused-at-budget"
                    ),
                    "delivery {index}: a delivered ticket either applies or is refused with a \
                     typed arm; got {arm}"
                );
                assert_eq!(
                    (after.attempts(), after.spend(), after.epoch()),
                    (before.attempts(), before.spend(), before.epoch()),
                    "delivery {index} ({arm}): a refused repair charges nothing and mints no \
                     epoch, however much the caller offered to spend"
                );
            }
            sim.record(&format!(
                "delivery {index} arm={arm} attempts={} spend={} epoch={}",
                after.attempts(),
                after.spend(),
                after.epoch()
            ));
        }
        assert_eq!(
            (applied, refused),
            (1, deliveries.saturating_sub(1)),
            "the first delivery of a ticket applies it and every later one is a typed \
             refusal, which is what makes the sequence above readable"
        );
        sim.record(&format!(
            "ordinal={} reach={} deliveries={deliveries} applied={applied}",
            plan.ordinal, plan.reach
        ));
        Ok(())
    })
}

/// T13: a spent budget is a finite typed refusal, a refused attempt charges
/// nothing, and the refusal is stable rather than a momentary one.
///
/// The two ceilings a host can be built with are drawn separately, because they
/// refuse at different points and a bug that moved one into the other would be
/// invisible if only one were swept; both spend the same number of *unit* attempts
/// against their own ceiling, so the refusal arrives after the same number of
/// attempts whichever ceiling binds. The attempts after the first are
/// [`Host::resume`]s rather than repairs, and that is the point rather than a
/// convenience: a repair whose ticket has already applied is refused `StaleEpoch`
/// at the door *before* the ledger is charged, so the budget arm is only reachable
/// through a plain attempt — which is exactly T13's claim that a permanent refusal
/// plus repeated `NotApplied` reaches a finite answer.
///
/// What is deliberately **not** claimed is that the two ceilings are
/// distinguishable from each other: both arrive as `BudgetSpent`, and a family
/// asserting otherwise would be asserting a distinction the type does not make.
#[test]
fn a_spent_budget_refuses_every_later_attempt() -> TestResult {
    sim::assert_replays(Band::new(120, 8), |sim| {
        let ceiling = u64::from(sim.rng().between(2, 5));
        // Every attempt — the blocked one and every resume — charges one unit of
        // spend and one attempt, so bounding the two by the same number is what
        // makes either ceiling bind on the same attempt and the two arms differ
        // only in which check fired.
        let over_attempts = sim.rng().below(2) == 0;
        let (attempts, spend) = if over_attempts {
            (ceiling, u64::MAX)
        } else {
            (u64::MAX, ceiling)
        };
        let dir = sim.scratch("repair-spent")?;
        let host = bounded_host(TENANTS[0], &dir, attempts, spend)?;
        let declared = shared::journey_task()?;
        let case = block_with(&host, 1)?;
        let opened = charged(&host, case.run)?;
        assert_eq!(
            opened.attempts(),
            1,
            "the blocked attempt charged one of the {ceiling} the run is allowed; the \
             attempts below are what is left of the ceiling"
        );

        // One attempt per remaining unit of budget until the ceiling refuses. The
        // loop is bounded by the ceiling plus two, so a budget that never refused
        // would fail here rather than run forever.
        let mut admitted = 0_usize;
        let mut arm = "succeeded";
        for index in 0..ceiling.saturating_add(2) {
            let before = charged(&host, case.run)?;
            let report =
                lgwks_bot::block_on(host.resume(case.run, &declared, input(Polls::shared(), 7)));
            arm = disposition_arm(report.disposition());
            let after = charged(&host, case.run)?;
            if arm == "refused-at-budget" {
                assert_eq!(
                    (after.attempts(), after.spend()),
                    (before.attempts(), before.spend()),
                    "attempt {index}: a refused attempt charged nothing at all, so the \
                     counters it would have moved are the ones it left"
                );
                break;
            }
            admitted = admitted.saturating_add(1);
            assert_eq!(
                after.attempts(),
                opened.attempts().saturating_add(u64::try_from(admitted)?),
                "attempt {index}: every admitted attempt is charged once, so the count the \
                 ceiling is measured against is the number of attempts that ran"
            );
            assert_eq!(
                after.spend(),
                after.attempts(),
                "attempt {index}: a plain attempt spends one unit and takes one attempt, so \
                 the two counters move together and either ceiling binds on the same one"
            );
        }
        assert_eq!(
            arm, "refused-at-budget",
            "a run past either ceiling reaches a finite typed refusal rather than retrying; \
             the last arm was {arm}"
        );
        assert_eq!(
            admitted,
            usize::try_from(ceiling.saturating_sub(1))?,
            "the blocked attempt took one of the {ceiling} attempts and each resume took one \
             of the rest, so the refusal lands on the attempt after the last one admitted"
        );
        let exhausted = charged(&host, case.run)?;
        assert!(
            exhausted.attempts() <= ceiling && exhausted.spend() <= ceiling,
            "no counter was pushed past the ceiling that refused: attempts={} spend={} against \
             attempts={attempts} spend={spend}",
            exhausted.attempts(),
            exhausted.spend()
        );

        // Two more attempts of the same run: the refusal is stable, not a single
        // lucky tick, and neither moves a counter.
        for _ in 0..2 {
            let again =
                lgwks_bot::block_on(host.resume(case.run, &declared, input(Polls::shared(), 7)));
            assert_eq!(
                disposition_arm(again.disposition()),
                "refused-at-budget",
                "a spent budget refuses every later attempt, not only the first one past the \
                 ceiling"
            );
            assert_eq!(
                charged(&host, case.run)?,
                exhausted,
                "a refused attempt charges nothing at all: the counters the ceiling was \
                 measured against never move afterwards"
            );
        }
        sim.record(&format!(
            "ceiling={ceiling} bound-attempts={over_attempts} admitted={admitted} attempts={} \
             spend={} epoch={} refused={}",
            exhausted.attempts(),
            exhausted.spend(),
            exhausted.epoch(),
            host.admission().refused()
        ));
        Ok(())
    })
}

/// T13: a run whose root budget is spent is refused for good, and the host keeps
/// repairing the runs whose budgets are not.
///
/// The recovery half of the saturation case, and the one that makes the refusal
/// meaningful: a budget that refused permanently would be a host that stopped
/// working, not a budget. The budget lives in a *per-run* ledger entry, so the
/// claim is that a spent run's ceiling reaches its finite refusal without touching
/// the other runs sharing the host — one that was blocked at the same time and one
/// started afterwards.
///
///
/// The refused attempts are [`Host::resume`]s rather than repairs, because that is
/// where T13's "permanent refusal plus repeated `NotApplied`" lives: a repair whose
/// ticket has already applied is refused `StaleEpoch` at the door before the ledger
/// is consulted. What this does **not** claim is that the poisoned handle stays
/// usable: a budget refusal drops the step store's handle with the refused append
/// outstanding, so recovery here is *through a reopen* — which is the only honest
/// form, and INV-BOT-50's poison is what makes it the only one.
#[test]
fn a_host_spent_on_one_run_still_repairs_the_next() -> TestResult {
    let dir = Scratch::new("repair-recovery")?;
    let host = bounded_host(TENANTS[0], dir.path(), ATTEMPT_CEILING, u64::MAX)?;
    let declared = shared::journey_task()?;

    let first = block_with(&host, candidate_needs().len())?;
    let second = block_with(&host, candidate_needs().len())?;
    assert_ne!(
        first.run, second.run,
        "two runs over one host mint two identities: the ceiling is per run, and two runs \
         sharing one identity would share one budget"
    );

    // The second run spends its budget on plain attempts: each resume takes one
    // attempt, and the next past the ceiling is the refusal — T13's "permanent \
    // refusal plus repeated NotApplied reaches a finite answer", driven at a stated \
    // ceiling rather than at a hand-written count.
    let mut admitted = 1_usize;
    let mut arm = disposition_arm(
        lgwks_bot::block_on(host.resume(second.run, &declared, input(Polls::shared(), 7)))
            .disposition(),
    );
    // A resume of a run with no authority is still `Blocked`, not admitted: the
    // admission check is in front of the budget charge, so "blocked" is the run
    // arriving under its own shortfall and "refused-at-budget" is the ceiling.
    while arm == "blocked" {
        admitted = admitted.saturating_add(1);
        assert!(
            admitted <= usize::try_from(ATTEMPT_CEILING)?,
            "a ceiling of {ATTEMPT_CEILING} refused nothing in {admitted} attempts: the bound \
             would be no bound at all"
        );
        arm = disposition_arm(
            lgwks_bot::block_on(host.resume(second.run, &declared, input(Polls::shared(), 7)))
                .disposition(),
        );
    }
    assert_eq!(
        arm, "refused-at-budget",
        "past the ceiling the run is refused rather than blocked again or retried; the \
         arm was {arm}"
    );
    let spent = charged(&host, second.run)?;
    assert_eq!(
        spent.attempts(),
        u64::try_from(admitted)?,
        "the refused attempt charged nothing: the run stands at the attempts it admitted"
    );
    assert_eq!(
        spent.epoch(),
        0,
        "a run that was never repaired never left epoch zero, however many attempts it took"
    );

    // Reopen. A budget refusal drops the step store's handle with the refused \
    // append outstanding — INV-BOT-50's poison — so recovery is *through a reopen* \
    // rather than through the poisoned handle, and the reopen has to replay both \
    // runs' budgets off the file to be worth anything.
    drop(host);
    let reopened = repairable_host(TENANTS[0], dir.path(), GrantSet::empty())?;
    assert_eq!(
        charged(&reopened, second.run)?,
        spent,
        "the refusal left the durable budget where it was, and a reopened ledger reads it \
         back byte for byte"
    );

    // The other run, blocked at the same time and sharing the directory, still closes.
    let repaired = lgwks_bot::block_on(reopened.repair(
        &first.ticket,
        &exact_grant(candidate_needs().len()),
        &declared,
        input(Polls::shared(), 7),
        1,
    ))?;
    assert_eq!(
        repaired.disposition(),
        Disposition::Succeeded,
        "a run that never exhausted its own budget still closes on a host whose other run \
         did: {:?}",
        repaired.error()
    );
    assert_eq!(
        charged(&reopened, first.run)?.epoch(),
        1,
        "the recovered run is at epoch one: the refused run's counters did not leak onto it"
    );
    assert_eq!(
        charged(&reopened, first.run)?.attempts(),
        2,
        "it charged one attempt for its block and one for this repair, from its own untouched \
         budget"
    );

    // And a run started after the refusal is still repairable.
    let third = block_with(&reopened, candidate_needs().len())?;
    let closed = lgwks_bot::block_on(reopened.repair(
        &third.ticket,
        &exact_grant(candidate_needs().len()),
        &declared,
        input(Polls::shared(), 7),
        1,
    ))?;
    assert_eq!(
        closed.disposition(),
        Disposition::Succeeded,
        "a run started after the refusal still closes, so the refusal bound one run and not \
         the host: {:?}",
        closed.error()
    );
    Ok(())
}

/// T13 at scale: over one ledger, at 100 and at 1,000 runs, every run gets its
/// own ticket and every ticket is applied exactly once.
///
/// One sweep rather than two families, because the property is a property of the
/// *chain* and a chain of a hundred is a chain. What it proves is that at a scale
/// no other family in this file reaches — one ledger holding a thousand runs'
/// charges — each ticket applied exactly once and each run charged exactly twice:
/// once for its block and once for its repair. The 10,000 tier of the declared
/// claim is [`TIERS`], measured when `LGWKS_REPAIR_SCALE` is set by `tiers.rs`; the
/// level this sweep reached is printed rather than inferred, which is the
/// INV-BOT-16 rule stated for this sweep.
#[test]
fn a_bounded_sweep_repairs_every_ticket_once() -> TestResult {
    for tier in SWEPT_TIERS {
        let dir = Scratch::new("repair-sweep")?;
        let host = blocked_host(TENANTS[0], dir.path())?;
        let declared = shared::journey_task()?;
        let grants = exact_grant(candidate_needs().len());

        let mut runs = Vec::with_capacity(tier);
        for _ in 0..tier {
            let case = block_with(&host, candidate_needs().len())?;
            runs.push((case.run, case.ticket, case.polls));
        }

        let mut applied = 0_usize;
        let mut refused = 0_usize;
        for (index, entry) in runs.iter().enumerate() {
            let run = entry.0;
            let repaired = lgwks_bot::block_on(host.repair(
                &entry.1,
                &grants,
                &declared,
                input(Arc::clone(&entry.2), 7),
                1,
            ));
            match arm_of(&repaired) {
                "succeeded" => applied = applied.saturating_add(1),
                "already-applied" | "stale-epoch" => refused = refused.saturating_add(1),
                arm => {
                    return Err(format!(
                        "tier {tier}: repair {index} must apply or be refused as a replay, \
                         got {arm}"
                    )
                    .into());
                }
            }
            assert_eq!(
                charged(&host, run)?.applied(),
                1,
                "tier {tier}: run {index} has one applied ticket and no more, however many \
                 times it was delivered"
            );
            assert_eq!(
                charged(&host, run)?.attempts(),
                2,
                "tier {tier}: run {index} was charged once for the block and once for the \
                 repair, and never a third time"
            );
        }
        assert_eq!(
            (applied, refused),
            (tier, 0),
            "tier {tier}: every ticket is delivered once here, so every one of them applies \
             and none is refused as a replay"
        );
        let mut line = std::io::stdout().lock();
        let _written = writeln!(
            line,
            "repair sweep tier {tier}: requested={tier} reached={applied} runs={} refused={refused} \
             admitted={} peaks={}",
            runs.len(),
            host.admission().admitted(),
            host.admission().peak_in_flight()
        );
    }
    Ok(())
}
