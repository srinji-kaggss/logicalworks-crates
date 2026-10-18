//! The logical clock's restart contract, run as tests (#526).
//!
//! `clock.rs` carried four doctests that named the crate by path. Each claim is a test here, so it
//! runs under nextest with the rest of the suite and the module documentation says what the test
//! holds instead of printing a program. The seeded sweep of the same contract is `sim_clock`.

use std::time::Duration;

use lgwks_bot::clock::Clock;

#[test]
fn a_restart_restores_the_spent_duration_and_the_budget_that_remains() {
    let clock = Clock::virtual_at(Duration::ZERO);
    let budget = Duration::from_secs(5);

    assert!(clock.advance(Duration::from_secs(2)).is_ok());
    let recorded = clock.snapshot();

    let resumed = Clock::virtual_at(recorded.elapsed());
    assert_eq!(
        resumed.snapshot().remaining_from(budget),
        Duration::from_secs(3)
    );
}

#[test]
fn a_wall_clock_restored_after_a_restart_keeps_the_time_already_spent() {
    let clock = Clock::wall_at(Duration::from_secs(4));
    assert!(clock.now() >= Duration::from_secs(4));
    assert!(
        clock.snapshot().remaining_from(Duration::from_secs(10)) <= Duration::from_secs(6),
        "at most six of the ten seconds remain"
    );
}

#[test]
fn a_virtual_clock_starts_where_it_was_placed() {
    let clock = Clock::virtual_at(Duration::from_secs(3));
    assert_eq!(clock.now(), Duration::from_secs(3));
}

#[test]
fn the_wall_watchdog_keeps_running_while_logical_time_moves_a_day() {
    let clock = Clock::virtual_at(Duration::ZERO);
    let watchdog = clock.wall_watchdog();
    assert!(clock.advance(Duration::from_secs(86_400)).is_ok());
    assert_eq!(clock.now(), Duration::from_secs(86_400));
    // A logical day passed in one call; the watchdog reads real time, so it has not moved with it.
    assert!(watchdog.elapsed() < Duration::from_secs(86_400));
}
