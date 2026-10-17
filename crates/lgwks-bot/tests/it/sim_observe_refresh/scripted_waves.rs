//! INV-BOT-120..124: how many watchdogs a wave spends, a cancelled tick, and a stalled chain's mark.

use super::*;

/// A wave whose every source answers on its first poll starts no watchdog at all.
///
/// The lazy half of INV-BOT-124 as a function of the width: at 1 chain and at 32
/// the ordinary tick pays nothing, because a poll that answered never reaches
/// the arm that starts a watcher. The widths are drawn, so the property is
/// checked across the wave rather than at one point in it.
#[test]
fn a_fast_wave_spends_no_watchdog_across_seeded_widths() -> TestResult {
    const SEED: u64 = 0x1240_0001;
    let mut rng = Rng::new(SEED);
    let ceiling = u32::try_from(WAVE_WIDTH)?;
    for _ in 0..6_u32 {
        let width = usize::try_from(rng.between(1, ceiling))?;
        let holds = Rc::new(Cell::new(false));
        let mut subject = scripted_bot("sim-observe-fast-wave", 0, width, SIM_DEADLINE, holds)?;
        for (index, held) in subject.handles.iter().enumerate() {
            let value = u32::try_from(index)?.saturating_add(1);
            arm_chain(held, value, 0, None, 0);
        }
        let _outcome = subject.bot.tick();
        let report = subject.bot.tick_report();
        assert_eq!(
            report.watchdogs(),
            0,
            "seed {SEED} width {width}: an ordinary tick — every source answered \
             on its first poll — starts no deadline thread at all"
        );
        assert!(
            report.stalled().is_empty(),
            "seed {SEED} width {width}: and reports no stall, because nothing was \
             given up on: {:?}",
            report.stalled()
        );
    }
    Ok(())
}

/// A source that yields once before it answers spends exactly one watchdog for
/// its wave, and the waves beside it spend none.
///
/// INV-BOT-124's other half. The bot is wider than one wave, and the seed picks
/// which chain yields, so the assertion is that *one* wave — the one holding the
/// poll that went `Pending` — spends a watcher, and the resolved waves do not.
/// A source that yields once is the case a per-poll watcher would miss and a
/// never-started one would also miss; only a lazy, per-wave watcher gets it.
#[test]
fn a_pending_source_spends_one_watchdog_for_its_wave() -> TestResult {
    const SEED: u64 = 0x1240_0002;
    let mut rng = Rng::new(SEED);
    let wave = u32::try_from(WAVE_WIDTH)?;
    let width = usize::try_from(rng.between(wave.saturating_add(1), wave.saturating_mul(2)))?;
    let yields = usize::try_from(rng.below(u32::try_from(width)?))?;
    let holds = Rc::new(Cell::new(false));
    let mut subject = scripted_bot("sim-observe-pending", 0, width, SIM_DEADLINE, holds)?;
    for (index, held) in subject.handles.iter().enumerate() {
        let pending = u32::from(index == yields);
        let value = u32::try_from(index)?.saturating_add(1);
        arm_chain(held, value, 0, None, pending);
    }
    let _outcome = subject.bot.tick();
    let report = subject.bot.tick_report();
    assert_eq!(
        report.watchdogs(),
        1,
        "seed {SEED}: width {width} is {} waves and chain {yields} yielded once \
         before answering, so exactly its wave spends one watcher and the rest \
         spend none",
        width.div_ceil(WAVE_WIDTH)
    );
    assert!(
        report.stalled().is_empty(),
        "seed {SEED}: the poll that yielded once answered before the budget, so \
         nothing is reported stalled: {:?}",
        report.stalled()
    );
    Ok(())
}

/// The watchdog count summed over a run equals the number of ticks whose wave
/// was left pending.
///
/// The aggregate form of the wave rule, read from the reports rather than
/// inferred from the schedule: a tick where every source answered spends zero
/// and a tick that left one poll pending spends one, so the two sums are one
/// fact. A counter that drifted — a watcher started but not counted, or counted
/// but never started — would separate them.
#[test]
fn a_seeded_run_spends_one_watchdog_per_pending_tick() -> TestResult {
    const SEED: u64 = 0x1240_0003;
    let mut rng = Rng::new(SEED);
    let ticks = rng.between(3, 6);
    let holds = Rc::new(Cell::new(false));
    let mut subject = scripted_bot("sim-observe-pending-ticks", 0, 3, SIM_DEADLINE, holds)?;
    let mut watchdogs = 0_u32;
    let mut pending_ticks = 0_u32;
    for tick in 1..=ticks {
        let mut pending = false;
        for (index, held) in subject.handles.iter().enumerate() {
            let wedged = rng.chance(350);
            pending = pending || wedged;
            let pace = if wedged { WEDGED_PACE } else { 0 };
            let value = tick
                .saturating_mul(10)
                .saturating_add(u32::try_from(index)?);
            arm_chain(held, value, pace, None, 0);
        }
        let _outcome = subject.bot.tick();
        let report = subject.bot.tick_report();
        let expected = u32::from(pending);
        assert_eq!(
            report.watchdogs(),
            expected,
            "seed {SEED} tick {tick}: a tick that left a poll pending spends one \
             watcher and a tick where every source answered spends none"
        );
        watchdogs = watchdogs.saturating_add(report.watchdogs());
        pending_ticks = pending_ticks.saturating_add(expected);
    }
    assert_eq!(
        watchdogs, pending_ticks,
        "seed {SEED}: the watchdog count summed over {ticks} ticks equals the \
         number of ticks with a pending wave, both read from the reports"
    );
    Ok(())
}

/// A tick dropped mid-wave leaves the bot usable, and the next tick reports
/// normally.
///
/// The cancellation INV-BOT-123 documents is between phases; this is the other
/// drop the tick's own contract names. The future is polled once — far enough
/// to enter the wave and park on a wedged source — and then dropped. What the
/// family asserts is that the world it left behind is not poisoned: the next
/// tick re-polls every source, commits, and reports no stall of its own.
#[test]
fn a_cancelled_tick_leaves_the_bot_usable() -> TestResult {
    const SEED: u64 = 0x1240_0004;
    let mut rng = Rng::new(SEED);
    let holds = Rc::new(Cell::new(false));
    let mut subject = baseline_bot("sim-observe-cancelled-tick", 3, SIM_DEADLINE, holds)?;

    let wedged = usize::try_from(rng.below(3))?;
    for (index, held) in subject.handles.iter().enumerate() {
        let pace = if index == wedged { WEDGED_PACE } else { 0 };
        arm_chain(held, 7, pace, None, 0);
    }
    // `Bot::tick` parks the thread until the future resolves, so the only way
    // to drop a tick mid-flight is to drive `tick_async` by hand. `Waker::noop`
    // is the legal waker that never needs to wake: the future is dropped before
    // anything would.
    let mut context = Context::from_waker(Waker::noop());
    {
        let mut future = std::pin::pin!(subject.bot.tick_async());
        let first = Future::poll(future.as_mut(), &mut context);
        assert!(
            matches!(first, Poll::Pending),
            "seed {SEED}: the wedged source parks, so the first poll of the tick \
             returns Pending with the wave still in flight"
        );
    }

    for (index, held) in subject.handles.iter().enumerate() {
        let value = u32::try_from(index)?.saturating_add(20);
        arm_chain(held, value, 0, None, 0);
    }
    let _recovered = subject.bot.tick();
    let report = subject.bot.tick_report();
    assert_eq!(
        report.watchdogs(),
        0,
        "seed {SEED}: the tick after a cancelled one is ordinary — every source \
         answered on its first poll and no watcher was needed"
    );
    assert!(
        report.stalled().is_empty(),
        "seed {SEED}: nothing is reported stalled after the cancelled tick was \
         dropped: {:?}",
        report.stalled()
    );
    assert!(
        subject.ran.borrow().iter().any(|entry| entry.0 == wedged),
        "seed {SEED}: the chain that was wedged during the cancelled tick \
         committed on the next tick, so the bot is usable rather than poisoned"
    );
    Ok(())
}

/// A wedged source keeps its baseline and its forced-refresh mark standing, and
/// the next tick re-polls it.
///
/// INV-BOT-123's stall rule and INV-BOT-120's mark rule, together: a
/// cancellation commits nothing, so the value the source was to replace is
/// still there and the declaration that made it unsound is still standing. The
/// recovery tick re-enters the source and commits the value it reports, which is
/// the difference between a deferral and a retirement.
#[test]
fn a_stalled_chain_keeps_its_mark_and_is_re_polled_next_tick() -> TestResult {
    const SEED: u64 = 0x1230_0005;
    let mut rng = Rng::new(SEED);
    let cause = CAUSES[usize::try_from(rng.below(4))?];
    let holds = Rc::new(Cell::new(false));
    let mut subject = baseline_bot("sim-observe-stall-mark", 3, SIM_DEADLINE, holds)?;

    for (index, held) in subject.handles.iter().enumerate() {
        if index == 0 {
            arm_chain(held, 1, WEDGED_PACE, Some(cause), 0);
        } else {
            arm_chain(held, 10, 0, None, 0);
        }
    }
    let _stalled = subject.bot.tick();
    let after = subject.bot.tick_report();
    assert_eq!(
        after.stalled().len(),
        1,
        "seed {SEED}: chain 0's poll was cancelled: {:?}",
        after.stalled()
    );
    assert!(
        after
            .forced()
            .iter()
            .any(|row| row.chain() == 0 && row.reason() == cause),
        "seed {SEED}: the forced-refresh mark survives the stall, naming chain 0 \
         and the cause it declared: {:?}",
        after.forced()
    );

    arm_chain(&subject.handles[0], 42, 0, None, 0);
    let _recovered = subject.bot.tick();
    assert!(
        subject
            .ran
            .borrow()
            .iter()
            .any(|entry| entry.0 == 0 && entry.1 == 42),
        "seed {SEED}: the next tick re-polled the stalled chain and committed the \
         value it reported: {:?}",
        subject.ran.borrow()
    );
    let recovered = subject.bot.tick_report();
    assert!(
        recovered.stalled().is_empty(),
        "seed {SEED}: the tick that read the source reports no stall: {:?}",
        recovered.stalled()
    );
    Ok(())
}
