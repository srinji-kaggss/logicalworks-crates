//! INV-BOT-120..124: deadline bounds, stall and refresh reports, attribution and the deadline-wave replay.

use super::sim_yield_rule::PendingTurn;
use super::*;

/// A per-poll deadline is accepted at both boundaries and refused one step past
/// the ceiling, seeded around the edges.
///
/// The bound is only a bound if it cannot be set to something that is not one.
/// The smallest positive budget is a bounded wait and is accepted; the declared
/// ceiling is accepted; zero — which cancels every poll before its first poll —
/// and anything past the ceiling are typed refusals naming the number, because
/// a clamped deadline is indistinguishable from the one the caller asked for.
#[test]
fn a_poll_deadline_around_both_edges_is_accepted_or_refused_at_build() -> TestResult {
    const SEED: u64 = 0x1230_0006;
    let mut rng = Rng::new(SEED);
    let slack = u64::from(rng.between(1, 1_000));

    let smallest = Duration::from_nanos(slack);
    let accepted = Bot::builder("sim-deadline-smallest")
        .with_poll_deadline(smallest)
        .with_effects(tenant_scope(0)?)
        .build(&GrantSet::empty());
    assert!(
        accepted.is_ok(),
        "seed {SEED}: a positive budget is a bound, so it is accepted: {accepted:?}"
    );

    let at_ceiling = Bot::builder("sim-deadline-ceiling")
        .with_poll_deadline(MAX_POLL_DEADLINE)
        .with_effects(tenant_scope(0)?)
        .build(&GrantSet::empty());
    assert!(
        at_ceiling.is_ok(),
        "seed {SEED}: the declared ceiling is itself accepted: {at_ceiling:?}"
    );

    let default = Bot::builder("sim-deadline-default")
        .with_effects(tenant_scope(0)?)
        .build(&GrantSet::empty());
    assert!(
        default.is_ok(),
        "seed {SEED}: the declared default is inside both bounds: {default:?}"
    );
    assert!(
        DEFAULT_POLL_DEADLINE > Duration::ZERO && DEFAULT_POLL_DEADLINE <= MAX_POLL_DEADLINE,
        "seed {SEED}: the default budget is a bounded one"
    );

    let zero = Bot::builder("sim-deadline-zero")
        .with_poll_deadline(Duration::ZERO)
        .with_effects(tenant_scope(0)?)
        .build(&GrantSet::empty());
    assert!(
        matches!(zero, Err(BotError::PollDeadlineUnbounded { deadline }) if deadline.is_zero()),
        "seed {SEED}: zero bounds nothing and is refused, naming the number: {zero:?}"
    );

    let past = MAX_POLL_DEADLINE.saturating_add(Duration::from_nanos(slack));
    let over = Bot::builder("sim-deadline-over")
        .with_poll_deadline(past)
        .with_effects(tenant_scope(0)?)
        .build(&GrantSet::empty());
    match over {
        Err(BotError::PollDeadlineExceeded { deadline, ceiling }) => {
            assert_eq!(
                deadline, past,
                "seed {SEED}: the refusal names the budget that was asked for"
            );
            assert_eq!(
                ceiling, MAX_POLL_DEADLINE,
                "seed {SEED}: and the ceiling that would have been accepted, so the \
                 repair is a number rather than a guess"
            );
        }
        other => {
            return Err(
                format!("seed {SEED}: a budget past the ceiling was accepted: {other:?}").into(),
            );
        }
    }
    Ok(())
}

/// Among a wave of scripted sources, only the chain the seed wedged is reported
/// stalled — named by its own chain, its own `domain_id` and the applied budget.
///
/// The per-chain half of INV-BOT-123's report: the stall is not a wave-level
/// fact garbled across the chains in it. A report that named the wave, or the
/// first chain, or a neighbour would pass a "something stalled" assertion and
/// fail this one.
#[test]
fn only_the_seeded_wedged_chain_is_reported_stalled() -> TestResult {
    const SEED: u64 = 0x1230_0007;
    let mut rng = Rng::new(SEED);
    let ceiling = u32::try_from(WAVE_WIDTH)?;
    let width = usize::try_from(rng.between(2, ceiling))?;
    let wedged = usize::try_from(rng.below(u32::try_from(width)?))?;
    let holds = Rc::new(Cell::new(false));
    let mut subject = scripted_bot("sim-observe-one-stalled", 0, width, SIM_DEADLINE, holds)?;
    for (index, held) in subject.handles.iter().enumerate() {
        let pace = if index == wedged { WEDGED_PACE } else { 0 };
        let value = u32::try_from(index)?.saturating_add(1);
        arm_chain(held, value, pace, None, 0);
    }
    let _outcome = subject.bot.tick();
    let report = subject.bot.tick_report();
    assert_eq!(
        report.stalled().len(),
        1,
        "seed {SEED} width {width}: exactly the chain the seed wedged is \
         reported: {:?}",
        report.stalled()
    );
    let row = report
        .stalled()
        .first()
        .ok_or_else(|| format!("seed {SEED}: the wedged chain is named"))?;
    assert_eq!(
        row.chain(),
        wedged,
        "seed {SEED}: the reported chain is the wedged one"
    );
    assert_eq!(
        row.domain(),
        DOMAINS[0][wedged % 8],
        "seed {SEED}: named by its own domain_id, not a neighbour's"
    );
    assert_eq!(
        row.deadline(),
        SIM_DEADLINE,
        "seed {SEED}: and carrying the budget that was actually applied"
    );
    Ok(())
}

/// The siblings of a wedged source commit and act in the *same* tick, at seeded
/// widths.
///
/// INV-BOT-123's "the chains beside it commit and act in the same tick", stated
/// where a tick boundary can be seen: a cancellation is not a park, so the tick
/// reaches the walk with every other chain's observation committed. Exactly one
/// sibling moves, so the change-selection rule cannot hide it, and the wedged
/// chain's own action must not appear.
#[test]
fn siblings_of_a_wedged_source_act_in_the_same_tick() -> TestResult {
    const SEED: u64 = 0x1230_0008;
    let mut rng = Rng::new(SEED);
    let width = usize::try_from(rng.between(3, 12))?;
    let wedged = usize::try_from(rng.below(u32::try_from(width)?))?;
    let mover = wedged.saturating_add(1) % width;
    let holds = Rc::new(Cell::new(false));
    let mut subject = scripted_bot("sim-observe-siblings", 0, width, SIM_DEADLINE, holds)?;
    for (index, held) in subject.handles.iter().enumerate() {
        let value = u32::try_from(index)?.saturating_add(1);
        arm_chain(held, value, 0, None, 0);
    }
    let _baseline = subject.bot.tick();
    let before = subject.ran.borrow().len();

    for (index, held) in subject.handles.iter().enumerate() {
        let pace = if index == wedged { WEDGED_PACE } else { 0 };
        let value = if index == mover {
            999
        } else {
            u32::try_from(index)?.saturating_add(1)
        };
        arm_chain(held, value, pace, None, 0);
    }
    let _stalled = subject.bot.tick();
    let ran = subject.ran.borrow().clone();
    let added: Vec<(usize, u32)> = ran.iter().skip(before).copied().collect();
    assert!(
        ran.len() > before,
        "seed {SEED} width {width}: the sibling that moved acted in the same tick \
         the wedged source was cancelled: {ran:?}"
    );
    assert!(
        added.iter().any(|entry| entry.0 == mover && entry.1 == 999),
        "seed {SEED}: the moving sibling acted with the value it moved to: {added:?}"
    );
    assert!(
        !added.iter().any(|entry| entry.0 == wedged),
        "seed {SEED}: the wedged chain committed nothing and acted nothing, \
         because its poll was cancelled: {added:?}"
    );
    Ok(())
}

/// Each chain's own declared cause is reported against that chain in `forced`.
///
/// INV-BOT-120's per-chain mark: the report is a bijection with the
/// declarations, so a chain that declared nothing is not forced and a chain
/// that declared a cause names its own cause — not a sibling's, and not a
/// bot-wide guess about which transport is up.
#[test]
fn a_seeded_reason_per_chain_is_reported_against_its_own_chain() -> TestResult {
    const SEED: u64 = 0x1200_0009;
    let mut rng = Rng::new(SEED);
    let holds = Rc::new(Cell::new(false));
    let mut subject = scripted_bot("sim-observe-reasons", 0, 3, SIM_DEADLINE, holds)?;
    let mut expected: Vec<(usize, RefreshReason)> = Vec::new();
    for (index, held) in subject.handles.iter().enumerate() {
        let cause = if rng.chance(700) {
            let cause = CAUSES[usize::try_from(rng.below(4))?];
            expected.push((index, cause));
            Some(cause)
        } else {
            None
        };
        let value = u32::try_from(index)?.saturating_add(1);
        arm_chain(held, value, 0, cause, 0);
    }
    let _outcome = subject.bot.tick();
    let report = subject.bot.tick_report();
    let got: Vec<(usize, RefreshReason)> = report
        .forced()
        .iter()
        .map(|row| (row.chain(), row.reason()))
        .collect();
    assert_eq!(
        got, expected,
        "seed {SEED}: the forced report is a bijection with the declarations — \
         each chain names its own cause and no undeclared chain is forced"
    );
    for row in report.forced() {
        assert_eq!(
            row.domain(),
            DOMAINS[0][row.chain() % 8],
            "seed {SEED}: chain {} is named by its own domain",
            row.chain()
        );
    }
    Ok(())
}

/// A forced refresh whose poll keeps failing stays marked until a read that
/// commits spends it.
///
/// The two halves of INV-BOT-120's spend rule that a single-tick family cannot
/// see: while every forced poll fails, the mark outlives each tick and the
/// report still names the chain; once the source answers, the committing read
/// spends the mark, and the tick after that is quiet. A repair that cleared the
/// mark on the failed poll would return the bot to comparing against the
/// unsound baseline it just failed to replace.
#[test]
fn a_failed_forced_refresh_keeps_the_mark_until_a_committed_read_spends_it() -> TestResult {
    const SEED: u64 = 0x1200_0010;
    let mut rng = Rng::new(SEED);
    let cause = CAUSES[usize::try_from(rng.below(4))?];
    let failures = rng.between(2, 4);
    let holds = Rc::new(Cell::new(false));
    let mut subject = scripted_bot("sim-observe-failed-mark", 0, 2, SIM_DEADLINE, holds)?;
    arm_chain(&subject.handles[0], 1, 0, Some(cause), 0);
    subject.handles[0].refusing.set(true);
    arm_chain(&subject.handles[1], 2, 0, None, 0);

    for tick in 1..=failures {
        let _outcome = subject.bot.tick();
        let report = subject.bot.tick_report();
        assert!(
            report
                .forced()
                .iter()
                .any(|row| row.chain() == 0 && row.reason() == cause),
            "seed {SEED} failure {tick} of {failures}: chain 0's forced poll \
             refused, so its mark stands and the report still names it: {:?}",
            report.forced()
        );
        assert!(
            report.stalled().is_empty(),
            "seed {SEED} failure {tick}: a refused poll is a domain failure, not a \
             cancellation: {:?}",
            report.stalled()
        );
    }

    subject.handles[0].refusing.set(false);
    subject.handles[0].declared.set(None);
    subject.handles[0].value.set(50);
    let _spend = subject.bot.tick();
    assert!(
        subject
            .ran
            .borrow()
            .iter()
            .any(|entry| entry.0 == 0 && entry.1 == 50),
        "seed {SEED}: the committed read spent the mark by acting on the value it \
         read: {:?}",
        subject.ran.borrow()
    );

    let _quiet = subject.bot.tick();
    let after = subject.bot.tick_report();
    assert!(
        after.forced().iter().all(|row| row.chain() != 0),
        "seed {SEED}: the mark was spent by the read that committed, so the quiet \
         tick does not force chain 0 again: {:?}",
        after.forced()
    );
    Ok(())
}

/// A seeded sequence of value changes under a held action reports each replaced
/// unacted revision exactly once, and never as a fired effect.
///
/// INV-BOT-121 in the shape a counter cannot express: the generation that first
/// acted on the value stays held, so every later value is committed and passed
/// over. `superseded` must name each replaced revision once — the intermediates
/// between the admitted first value and the newest — and `fired` must count
/// none of them, because a pass-over is not work done.
#[test]
fn a_seeded_value_sequence_under_a_held_action_reports_each_replaced_revision_once() -> TestResult {
    const SEED: u64 = 0x1210_0011;
    let mut rng = Rng::new(SEED);
    let steps = rng.between(3, 6);
    let holds = Rc::new(Cell::new(true));
    let mut subject = scripted_bot(
        "sim-observe-superseded",
        0,
        1,
        SIM_DEADLINE,
        Rc::clone(&holds),
    )?;
    let mut values: Vec<u32> = Vec::new();
    for step in 0..steps {
        values.push(step.saturating_mul(7).saturating_add(10));
    }

    let mut superseded: Vec<u64> = Vec::new();
    let mut fired = 0_usize;
    for value in &values {
        arm_chain(&subject.handles[0], *value, 0, None, 0);
        let _outcome = subject.bot.tick();
        let report = subject.bot.tick_report();
        fired = fired.saturating_add(report.fired());
        assert!(
            !report.forced_any(),
            "seed {SEED}: a pass-over is not a forced refresh: {:?}",
            report.forced()
        );
        for row in report.superseded() {
            superseded.push(row.revision());
        }
    }

    let expected: Vec<u64> = (2..u64::from(steps)).collect();
    assert_eq!(
        superseded, expected,
        "seed {SEED}: each replaced unacted revision is reported exactly once, in \
         order — the intermediates between the first, admitted value and the \
         newest"
    );
    assert_eq!(
        subject.ran.borrow().clone(),
        vec![(0, values[0])],
        "seed {SEED}: the held generation ran only on the first value, so no \
         superseded value is counted as fired"
    );
    assert_eq!(
        fired, 0,
        "seed {SEED}: no effect was counted as fired — the only generation that \
         ran reported its outcome unknown, and a pass-over is never counted as \
         either"
    );
    Ok(())
}

/// A seeded mass of held chains never starves an independent chain, at every
/// tier the host can build.
///
/// INV-BOT-122's saturation claim on one bot: the independent chain is declared
/// first and its effect lands, while every held chain occupies a transition the
/// walk reaches and cannot drop. The independent chain still runs, every held
/// chain still reaches its own action once, and every one is still *reported*
/// as held — a dropped hold is a lost effect nobody would see. The tier reached
/// is recorded rather than clamped.
#[test]
fn a_seeded_mass_of_held_chains_never_starves_an_independent_chain() -> TestResult {
    const SEED: u64 = 0x1220_0012;
    let mut rng = Rng::new(SEED);
    let bias = rng.below(64);
    let mut reached: Vec<usize> = Vec::new();
    for requested in [100_usize, 1_000, 10_000] {
        let independent = Rc::new(RefCell::new(Vec::new()));
        let held_seen = Rc::new(RefCell::new(Vec::new()));
        let first = ChainHandles::new();
        arm_chain(&first, bias.saturating_add(1), 0, None, 0);
        let mut builder = Bot::builder("sim-observe-mass")
            .with_poll_deadline(SIM_DEADLINE)
            .observe(scripted(&first, 0, 0))
            .on(
                |observed: &u32| *observed > 0,
                Logs {
                    ran: Rc::clone(&independent),
                    chain: 0,
                    holds: Rc::new(Cell::new(false)),
                },
            );
        for chain in 1..=requested {
            let held = ChainHandles::new();
            arm_chain(&held, bias.saturating_add(2), 0, None, 0);
            builder = builder.observe(scripted(&held, chain, 0)).on(
                |observed: &u32| *observed > 0,
                Logs {
                    ran: Rc::clone(&held_seen),
                    chain,
                    holds: Rc::new(Cell::new(true)),
                },
            );
        }
        let mut bot = builder
            .with_effects(tenant_scope(0)?)
            .build(&GrantSet::empty())?;
        let _outcome = bot.tick();
        assert_eq!(
            independent.borrow().len(),
            1,
            "seed {SEED} tier {requested}: the independent chain's action ran while \
             {requested} held chains occupied the walk"
        );
        assert_eq!(
            held_seen.borrow().len(),
            requested,
            "seed {SEED} tier {requested}: every held chain reached its own action \
             once: a walk that stopped at the first hold would have run one"
        );
        assert_eq!(
            bot.pending().len(),
            requested,
            "seed {SEED} tier {requested}: and every one of them is still reported \
             as held rather than dropped to make room"
        );
        assert_eq!(
            bot.source_domains().len(),
            requested.saturating_add(1),
            "seed {SEED} tier {requested}: the bot declares one independent chain and \
             {requested} held ones"
        );
        reached.push(requested);
    }
    assert_eq!(
        reached,
        vec![100, 1_000, 10_000],
        "seed {SEED}: the tiers this run reached, recorded rather than clamped, so \
         a reader is never told a concurrency number nobody ran"
    );
    Ok(())
}

/// Two tenants interleaved over one host never cross stalled, forced or
/// superseded attribution.
///
/// Two bots of different widths, so a chain index that crossed is detectable: a
/// five-chain tenant's chain 4 is impossible in a three-chain one. Each tenant
/// forces one chain, wedges another and holds its actions, so all three report
/// kinds are live on every tick and the assertion is not vacuous.
#[test]
fn two_tenants_interleaved_ticks_never_cross_attribution() -> TestResult {
    const SEED: u64 = 0x1250_0013;
    let mut rng = Rng::new(SEED);
    let ticks = rng.between(3, 6);
    let holds_left = Rc::new(Cell::new(true));
    let holds_right = Rc::new(Cell::new(true));
    let mut left = scripted_bot("sim-observe-two-left", 0, 3, SIM_DEADLINE, holds_left)?;
    let mut right = scripted_bot("sim-observe-two-right", 1, 5, SIM_DEADLINE, holds_right)?;
    let mut superseded_seen = 0_usize;

    for tick in 1..=ticks {
        for subject in [&mut left, &mut right] {
            let cause = CAUSES[usize::try_from(rng.below(4))?];
            for (index, held) in subject.handles.iter().enumerate() {
                let pace = if index == 1 { WEDGED_PACE } else { 0 };
                let declared = if index == 0 { Some(cause) } else { None };
                let value = tick
                    .saturating_mul(10)
                    .saturating_add(u32::try_from(index)?);
                arm_chain(held, value, pace, declared, 0);
            }
            let _outcome = subject.bot.tick();
            let report = subject.bot.tick_report();
            let prefix = format!("sim::t{}", subject.tenant);
            let width = subject.handles.len();
            assert!(
                report.forced().iter().any(|row| row.chain() == 0),
                "seed {SEED} tick {tick}: tenant {} forced its declared chain",
                subject.tenant
            );
            assert!(
                report.stalled().iter().any(|row| row.chain() == 1),
                "seed {SEED} tick {tick}: tenant {} stalled its wedged chain",
                subject.tenant
            );
            for row in report.forced() {
                assert!(
                    row.domain().starts_with(&prefix),
                    "seed {SEED} tick {tick}: tenant {} reported a forced source \
                     belonging to another tenant: {}",
                    subject.tenant,
                    row.domain()
                );
                assert!(
                    row.chain() < width,
                    "seed {SEED} tick {tick}: tenant {} reported a forbidden chain {}",
                    subject.tenant,
                    row.chain()
                );
            }
            for row in report.stalled() {
                assert!(
                    row.domain().starts_with(&prefix),
                    "seed {SEED} tick {tick}: tenant {} reported a stalled source \
                     belonging to another tenant: {}",
                    subject.tenant,
                    row.domain()
                );
            }
            for row in report.superseded() {
                assert!(
                    row.chain() < width,
                    "seed {SEED} tick {tick}: tenant {} reported a superseded chain \
                     it does not declare: {}",
                    subject.tenant,
                    row.chain()
                );
                superseded_seen = superseded_seen.saturating_add(1);
            }
        }
    }
    assert!(
        superseded_seen > 0,
        "seed {SEED}: the run exercised supersession, so the cross-tenant \
         assertion about it is not vacuous"
    );
    Ok(())
}

/// The deadline and watchdog families replay exactly, twice over.
///
/// `assert_replays` sweeps the band twice and compares the two hash vectors, so
/// a run that decided a different stall set — or started a different number of
/// watchers — fails even though every assertion above passed. The trace records
/// the *set* of stalled chains and the watchdog count rather than any timing, so
/// the receipt is a statement about what the run decided and not about how fast
/// it decided it.
///
/// Only the first chain to go `Pending` is armed to yield ([`PendingTurn`]): it
/// starts the wave's reaper, and a chain turned after it can find the wall
/// deadline spent (INV-BOT-123) and be reported stalled on scheduling alone, so
/// the trace would record the host and not the run (main run 39060016758).
#[test]
fn the_same_seed_replays_a_deadline_wave() -> TestResult {
    sim::assert_replays(sim::band_of(40), |sim| {
        let holds = Rc::new(Cell::new(false));
        let mut subject = scripted_bot("sim-observe-deadline-replay", 0, 3, SIM_DEADLINE, holds)?;
        let ticks = sim.rng().between(2, 3);
        for tick in 1..=ticks {
            let mut turn = PendingTurn::default();
            for (index, held) in subject.handles.iter().enumerate() {
                let wedged = sim.rng().chance(250);
                let yields = turn.arm(wedged, sim.rng().between(0, 1));
                let pace = if wedged { WEDGED_PACE } else { 0 };
                let value = tick
                    .saturating_mul(10)
                    .saturating_add(u32::try_from(index)?);
                arm_chain(held, value, pace, None, yields);
            }
            let _outcome = subject.bot.tick();
            let report = subject.bot.tick_report();
            sim.record(&format!("tick {tick} watchdogs {}", report.watchdogs()));
            for row in report.stalled() {
                sim.record(&format!("stalled {} {}", row.chain(), row.domain()));
            }
        }
        for entry in subject.ran.borrow().iter() {
            sim.record(&format!("ran {} {}", entry.0, entry.1));
        }
        Ok(())
    })
}
