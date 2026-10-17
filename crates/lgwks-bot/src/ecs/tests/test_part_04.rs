//! ECS bot unit tests, part 04: the poll watchdog's expiry rules (INV-BOT-123).

use super::*;
use crate::ecs::watchdog::bounded_wave;

/// A wave poll over `poll` in slot `slot`, as chain `slot`, not yet turned.
fn wave_poll<'wave>(
    watchdog: &'wave PollWatchdog,
    slot: usize,
    poll: impl Future<Output = Result<Option<Erased>, BotError>> + 'wave,
) -> WavePoll<'wave> {
    WavePoll {
        chain: slot,
        watchdog,
        slot,
        installed: false,
        poll: Box::pin(poll),
    }
}

/// A source that yields once, turned on a wave that expires inside that very
/// turn, answers on its next turn instead of being cut off by the `Pending` it
/// just returned.
///
/// The reaper is another thread, so on a loaded host the deadline can pass
/// before a poll's `Pending` has been registered. The poll's waker was
/// installed while the wave was live, so the reaper woke it and the poll is owed
/// one more turn (INV-BOT-123). Reading the expiry right after `Pending` and
/// stalling on it reported a source that was ready to answer as stalled on
/// nothing but how the host scheduled two threads, and the same seed decided a
/// different stall set (the main run after PR 586, `Tests (lgwks-bot full,
/// lgwks-std full)`, `sim_observe_refresh::the_same_seed_replays_a_deadline_wave`).
/// The expiry is written from inside the poll, after the turn installed its
/// waker, so no clock decides the interleaving.
#[test]
fn an_expiry_inside_a_first_turn_does_not_stall_a_source_ready_on_its_next() -> TestResult {
    let deadline = Duration::from_secs(60);
    let watchdog = PollWatchdog::new(deadline, 1);
    let clock = Clock::wall();
    let armed = AtomicBool::new(false);
    let mut cx = Context::from_waker(Waker::noop());
    let turns = Rc::new(Cell::new(0_u32));
    let mut yielding = wave_poll(
        &watchdog,
        0,
        std::future::poll_fn({
            let turns = Rc::clone(&turns);
            let shared = &watchdog;
            move |turn: &mut Context<'_>| {
                let seen = turns.get();
                turns.set(seen.saturating_add(1));
                if seen == 0 {
                    // What the reaper writes when the deadline passes, after this
                    // turn installed its waker: the wave is now expired.
                    crate::journal::owner::lock(&shared.state).expired = true;
                    turn.waker().wake_by_ref();
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(None))
                }
            }
        }),
    );
    assert!(
        yielding
            .turn(&mut cx, &clock, &armed, deadline)
            .is_pending(),
        "a poll whose waker was installed before the expiry is owed one more turn, not a stall"
    );
    assert!(
        matches!(
            yielding.turn(&mut cx, &clock, &armed, deadline),
            Poll::Ready(Ok(None))
        ),
        "the source ready on its next turn answers rather than stalls"
    );
    assert_eq!(turns.get(), 2, "the source was turned exactly twice");
    Ok(())
}

/// A poll whose first turn comes after the wave expired is cut off when that
/// turn leaves it pending.
///
/// Its waker is installed after the reaper's wake pass, so nothing would ever
/// wake it again: a wedged source here has to be stalled on the turn it is
/// first polled, or the tick never returns. This is the half of INV-BOT-123 the
/// first-turn case above must not trade away.
#[test]
fn a_poll_first_turned_after_expiry_is_cut_off_when_still_pending() -> TestResult {
    let deadline = Duration::from_secs(60);
    let watchdog = PollWatchdog::new(deadline, 1);
    let clock = Clock::wall();
    let armed = AtomicBool::new(false);
    let mut cx = Context::from_waker(Waker::noop());
    crate::journal::owner::lock(&watchdog.state).expired = true;
    let mut wedged = wave_poll(&watchdog, 0, std::future::pending());
    assert!(
        matches!(
            wedged.turn(&mut cx, &clock, &armed, deadline),
            Poll::Ready(Err(BotError::PollStalled { chain: 0, .. }))
        ),
        "a wedged source first polled after the expiry is cut off on that turn"
    );
    assert!(
        !armed.load(Ordering::Relaxed),
        "an expired wave starts no reaper"
    );
    Ok(())
}

/// Across many real waves under a deadline the reaper spends the moment it
/// starts, a source that yields once ahead of a wedged one always answers, and
/// the wedged one is always the one cut off.
///
/// The two cases above force the interleaving by hand; this one lets the real
/// reaper thread race the polling thread through [`bounded_wave`], the function
/// `Bot::tick` awaits. Its outcome cannot depend on who wins the race, because a
/// stall set that moves with the host's scheduling is the defect: on the code
/// that read the expiry right after `Pending`, a fraction of these waves stalled
/// the yielding source.
#[test]
fn a_yielding_source_ahead_of_a_wedge_always_answers_across_real_waves() -> TestResult {
    const WAVES: u32 = 1_000;
    let deadline = Duration::from_nanos(1);
    for wave in 0..WAVES {
        let watchdog = PollWatchdog::new(deadline, 2);
        let yielded = Rc::new(Cell::new(false));
        let yielding = wave_poll(
            &watchdog,
            0,
            std::future::poll_fn({
                let yielded = Rc::clone(&yielded);
                move |turn: &mut Context<'_>| {
                    if yielded.replace(true) {
                        Poll::Ready(Ok(None))
                    } else {
                        turn.waker().wake_by_ref();
                        Poll::Pending
                    }
                }
            }),
        );
        let wedged = wave_poll(&watchdog, 1, std::future::pending());
        let (results, armed) =
            lgwks_std::task::block_on(bounded_wave(deadline, &watchdog, [yielding, wedged]));
        assert!(
            matches!(results.first(), Some(Ok(None))),
            "wave {wave}: the source ready on its next turn answered"
        );
        assert!(
            matches!(
                results.get(1),
                Some(Err(BotError::PollStalled { chain: 1, .. }))
            ),
            "wave {wave}: the wedged source was cut off"
        );
        assert!(armed, "wave {wave}: a pending poll started the reaper");
    }
    Ok(())
}
