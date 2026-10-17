//! ECS bot unit tests, part 05: the quiet tick settles without the schedule (#279).

use super::*;

/// A bot over one source that reports a revision, and the cells that move it.
fn rev_bot(
    value: &Rc<Cell<u64>>,
    rev: &Rc<Cell<u64>>,
    runs: &Rc<Cell<usize>>,
) -> Result<EcsBot, Box<dyn std::error::Error>> {
    Ok(EcsBot::builder("quiet-settle")
        .observe(Rev {
            value: Rc::clone(value),
            rev: Rc::clone(rev),
            polls: Rc::new(Cell::new(0)),
            caps: vec![Cap::net()],
        })
        .on(|_: &u64| true, CountU64(Rc::clone(runs)))
        .with_effects(test_effects()?)
        .build(&net_grants())?)
}

/// A tick that polled nothing, holds nothing and is not parked runs no system.
///
/// Requirement (#279, the quiet tick): both of the schedule's systems would only
/// write "nothing moved" and an empty plan, and dispatching them was 282 ns of a
/// 907 ns tick (`bench/bot-profile.md`). Every system a schedule runs takes a new
/// change tick from the world, so a quiet tick that leaves the world's tick where it
/// found it ran none. The movement after the quiet ticks is the other half: it must
/// still fire exactly once, and a second quiet stretch must settle again, because
/// the change query compares against the system's own last run and the skipped
/// runs wrote no `Revision` for it to miss.
#[test]
fn a_quiet_tick_runs_no_system_and_a_movement_after_it_still_fires() -> TestResult {
    let value = Rc::new(Cell::new(1_u64));
    let rev = Rc::new(Cell::new(1_u64));
    let runs = Rc::new(Cell::new(0_usize));
    let mut bot = rev_bot(&value, &rev, &runs)?;

    // The first tick reads the value and opens the work it owes; the second
    // settles it. After that the source holds still.
    assert_eq!(bot.tick()?, 1, "the first observation fires once");
    let _second = bot.tick()?;
    let settled = bot.world.read_change_tick().get();
    for tick in 0..4 {
        assert_eq!(bot.tick()?, 0, "quiet tick {tick} fired");
    }
    assert_eq!(
        bot.world.read_change_tick().get(),
        settled,
        "four quiet ticks must dispatch no system: the world's change tick moved"
    );

    // A movement takes the full path, fires once, and is quiet again after.
    value.set(2);
    rev.set(2);
    assert_eq!(bot.tick()?, 1, "a movement after skipped ticks fires once");
    for tick in 0..3 {
        assert_eq!(bot.tick()?, 0, "tick {tick} after the movement fired again");
    }
    assert_eq!(
        runs.get(),
        2,
        "one run per observation, none for quiet ticks"
    );

    // And a second movement, after a second quiet stretch, is still seen.
    value.set(3);
    rev.set(3);
    assert_eq!(bot.tick()?, 1, "a second movement fires once");
    assert_eq!(runs.get(), 3, "the action ran once per movement");
    Ok(())
}
