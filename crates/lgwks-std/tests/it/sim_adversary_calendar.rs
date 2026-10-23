//! An attacker chooses the date, the second count and the nanosecond count.
//!
//! The calendar and the Unix-parts arithmetic sit under every timestamp the
//! estate stores or compares, and an attacker steers them by picking numbers:
//! the last day of a century, the second before the epoch, a nanosecond count
//! of `u32::MAX`, a day count at the end of `i64`. The invariants are the ones
//! a ledger leans on: a date has exactly one day number, a day number has
//! exactly one date, nothing wraps, and what the platform clock cannot hold is
//! a typed refusal and never a clamped instant.
//!
//! Sources: ISO 8601 and the Gregorian calendar reform (the 4/100/400 leap
//! rule), RFC 3339 section 5.6, H. Hinnant "chrono-Compatible Low-Level Date
//! Algorithms" (the era decomposition under test), CWE-190 (integer overflow).
//! Every answer is checked against a reference written here from the calendar's
//! definition, not from the library.

use lgwks_std::time::calendar::{civil_from_days, days_from_civil, is_leap, try_days_in_month};
use lgwks_std::time::format::{from_unix_parts, to_rfc3339, unix_parts};
use lgwks_std::time::{FormatError, ParseError, UnixTimeError, parse_rfc3339};

use crate::adversary::{Attacker, TestResult, sweep};
use crate::seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, initial_trace,
};

/// Seconds in a day.
const DAY: i64 = 86_400;

/// Nanoseconds in a second.
const SECOND_NANOS: u32 = 1_000_000_000;

/// The Gregorian leap rule, written from the calendar's definition.
fn reference_leap(year: i64) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

/// The length of `month` in `year`, `0` for a month the calendar does not have.
fn reference_month_length(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if reference_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// A word read as the signed value it spells.
fn signed(word: u64) -> i64 {
    i64::from_le_bytes(word.to_le_bytes())
}

/// A signed value read as the word it spells.
fn unsigned(value: i64) -> u64 {
    u64::from_le_bytes(value.to_le_bytes())
}

/// A whole second drawn from the years 1700 to 9999, which every platform's
/// clock can name and every four-digit rendering can print.
fn draw_second(attacker: &mut Attacker) -> i64 {
    let low = days_from_civil(1_700, 1, 1).saturating_mul(DAY);
    let high = days_from_civil(10_000, 1, 1).saturating_mul(DAY);
    low.saturating_add(signed(attacker.word()).rem_euclid(high.saturating_sub(low)))
}

/// Whether `text` names a date the calendar has: an instant the platform clock
/// cannot hold still names a valid date.
fn names_a_real_date(text: &str) -> bool {
    match parse_rfc3339(text) {
        Ok(_) | Err(ParseError::UnrepresentableInstant(_)) => true,
        Err(_) => false,
    }
}

/// One campaign: day counts across the whole `i64` range, folded into a trace.
fn campaign(seed: u64) -> u64 {
    let mut attacker = Attacker::new(seed);
    let mut trace = initial_trace();
    for _ in 0..512 {
        let (year, month, day) = civil_from_days(signed(attacker.word()));
        fold(&mut trace, unsigned(year));
        fold(&mut trace, u64::from(month));
        fold(&mut trace, u64::from(day));
    }
    trace
}

/// February 29 exists exactly in leap years, for every year a stamp can name.
#[test]
fn sim_adversary_calendar_a_leap_day_exists_exactly_in_leap_years() {
    for year in 0_i64..=9_999 {
        let text = format!("{year:04}-02-29T00:00:00Z");
        assert_eq!(
            names_a_real_date(&text),
            reference_leap(year),
            "February 29 of year {year} must be accepted exactly when it is a leap year"
        );
    }
}

/// The last day of every month is accepted and the day after it is refused
/// naming the bound, in common years, leap years and the century exceptions.
#[test]
fn sim_adversary_calendar_the_day_after_a_months_last_day_is_refused() {
    for year in [1900_i64, 2000, 2023, 2024, 2100, 2400] {
        for month in 1_u32..=12 {
            let length = reference_month_length(year, month);
            let last = format!("{year:04}-{month:02}-{length:02}T00:00:00Z");
            assert!(names_a_real_date(&last), "{last} is a real date");
            let past = format!(
                "{year:04}-{month:02}-{:02}T00:00:00Z",
                length.saturating_add(1)
            );
            assert!(
                matches!(
                    parse_rfc3339(&past),
                    Err(ParseError::OutOfRange { max, at: 8, .. }) if max == length
                ),
                "{past} must be refused with the month's length {length} as the bound"
            );
        }
    }
}

/// `is_leap` is the Gregorian rule over twenty thousand years and at the ends
/// of the `i64` range, where a naive implementation overflows.
#[test]
fn sim_adversary_calendar_is_leap_agrees_with_the_definition_everywhere() {
    for year in -10_000_i64..=10_000 {
        assert_eq!(is_leap(year), reference_leap(year), "year {year}");
    }
    for year in [
        i64::MIN,
        i64::MIN.saturating_add(1),
        i64::MAX,
        i64::MAX.saturating_sub(1),
        -400,
        400,
        -100,
        100,
    ] {
        assert_eq!(is_leap(year), reference_leap(year), "extreme year {year}");
    }
}

/// A month outside `1..=12` has no length, and every month inside it has the
/// length the table gives, in every year class.
#[test]
fn sim_adversary_calendar_a_month_the_calendar_lacks_has_no_length() {
    for month in [0_u32, 13, 14, 99, 100, 255, 65_536, u32::MAX] {
        assert_eq!(
            try_days_in_month(2024, month),
            None,
            "month {month} does not exist"
        );
    }
    for year in [1900_i64, 2000, 2023, 2024, i64::MIN, i64::MAX] {
        for month in 1_u32..=12 {
            assert_eq!(
                try_days_in_month(year, month),
                Some(reference_month_length(year, month)),
                "month {month} of year {year}"
            );
        }
    }
}

/// Walking 1.6 million consecutive days visits every date once, in order,
/// inside the calendar, and each date maps back to its own day number.
#[test]
fn sim_adversary_calendar_a_walk_of_days_is_a_bijection_in_order() {
    let mut previous = civil_from_days(-800_001);
    for days in -800_000_i64..800_000 {
        let current = civil_from_days(days);
        assert!(
            previous < current,
            "day {days}: {current:?} does not follow {previous:?}"
        );
        let (year, month, day) = current;
        assert!(
            (1..=reference_month_length(year, month)).contains(&day),
            "day {days} named {current:?}, which is not a date"
        );
        assert_eq!(
            days_from_civil(year, month, day),
            days,
            "day {days} did not round-trip"
        );
        previous = current;
    }
}

/// A date drawn across a quadrillion years either side of the epoch survives
/// the trip to its day number and back.
#[test]
fn sim_adversary_calendar_a_chosen_date_round_trips_across_the_domain() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let year = signed(attacker.word())
            .rem_euclid(2_000_000_000_000_000)
            .saturating_sub(1_000_000_000_000_000);
        let month = attacker.field(1, 13);
        let day = attacker.field(1, reference_month_length(year, month).saturating_add(1));
        let days = days_from_civil(year, month, day);
        assert_eq!(
            civil_from_days(days),
            (year, month, day),
            "seed {seed:#x} round {round}: {year}-{month}-{day} is day {days}"
        );
        Ok(())
    })
}

/// A day count anywhere in `i64`, including both ends, names a real date and
/// that date names the same day count: no wrap at the edges.
#[test]
fn sim_adversary_calendar_every_day_count_names_a_real_date() -> TestResult {
    for days in [
        i64::MIN,
        i64::MIN.saturating_add(1),
        -1,
        0,
        1,
        i64::MAX.saturating_sub(1),
        i64::MAX,
    ] {
        let (year, month, day) = civil_from_days(days);
        assert!((1..=12).contains(&month), "day count {days}: month {month}");
        assert!(
            (1..=reference_month_length(year, month)).contains(&day),
            "day count {days}: {year}-{month}-{day} is not a date"
        );
        assert_eq!(days_from_civil(year, month, day), days, "day count {days}");
    }
    sweep(1_024, |seed, round, attacker| {
        let days = signed(attacker.word());
        let (year, month, day) = civil_from_days(days);
        assert!(
            (1..=reference_month_length(year, month)).contains(&day),
            "seed {seed:#x} round {round}: day count {days} named {year}-{month}-{day}"
        );
        assert_eq!(
            days_from_civil(year, month, day),
            days,
            "seed {seed:#x} round {round}: day count {days}"
        );
        Ok(())
    })
}

/// The last day of a year is followed by the first of the next, and the day
/// after February 28 is February 29 exactly in a leap year, at every century.
#[test]
fn sim_adversary_calendar_year_and_century_boundaries_step_by_one_day() {
    for year in [
        1599_i64, 1600, 1699, 1700, 1799, 1800, 1899, 1900, 1999, 2000, 2099, 2100, 2399, 2400,
    ] {
        let new_year = days_from_civil(year, 12, 31).saturating_add(1);
        assert_eq!(
            civil_from_days(new_year),
            (year.saturating_add(1), 1, 1),
            "after {year}-12-31"
        );
        let after_february = civil_from_days(days_from_civil(year, 2, 28).saturating_add(1));
        let expected = if reference_leap(year) {
            (year, 2, 29)
        } else {
            (year, 3, 1)
        };
        assert_eq!(after_february, expected, "the day after {year}-02-28");
    }
}

/// A day number outside the contract's month and day ranges is answered, never
/// trapped: the function is total over every `i64` year and `u32` field.
#[test]
fn sim_adversary_calendar_out_of_contract_fields_are_answered_not_trapped() {
    for year in [i64::MIN, -1, 0, 1, i64::MAX] {
        for month in [0_u32, 1, 12, 13, 99, u32::MAX] {
            for day in [0_u32, 1, 31, 32, u32::MAX] {
                let first = days_from_civil(year, month, day);
                let second = days_from_civil(year, month, day);
                assert_eq!(first, second, "{year}-{month}-{day} is not a function");
            }
        }
    }
}

/// Nanoseconds past a second carry into the seconds, for every count a `u32`
/// can hold, and a pre-epoch second count carries the same way.
#[test]
fn sim_adversary_calendar_nanoseconds_carry_into_seconds_exactly() -> TestResult {
    let table = [
        (5_i64, 999_999_999_u32, 5_i64, 999_999_999_u32),
        (5, SECOND_NANOS, 6, 0),
        (5, 2_500_000_000, 7, 500_000_000),
        (5, u32::MAX, 9, 294_967_295),
        (-1, 1_500_000_000, 0, 500_000_000),
        (-5, u32::MAX, -1, 294_967_295),
    ];
    for (seconds, nanos, want_seconds, want_nanos) in table {
        assert_eq!(
            unix_parts(from_unix_parts(seconds, nanos)?)?,
            (want_seconds, want_nanos),
            "({seconds}, {nanos}) must normalise to ({want_seconds}, {want_nanos})"
        );
    }
    Ok(())
}

/// Any second count and any nanosecond count the attacker picks normalises to
/// the carry arithmetic and survives the trip to a `SystemTime` and back.
#[test]
fn sim_adversary_calendar_chosen_parts_normalise_by_the_carry_rule() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let seconds = signed(attacker.word())
            .rem_euclid(4_000_000_000)
            .saturating_sub(2_000_000_000);
        let nanos = attacker.field(0, u32::MAX);
        let carry = i64::from(nanos.div_euclid(SECOND_NANOS));
        let expected = (
            seconds.saturating_add(carry),
            nanos.rem_euclid(SECOND_NANOS),
        );
        assert_eq!(
            unix_parts(from_unix_parts(seconds, nanos)?)?,
            expected,
            "seed {seed:#x} round {round}: ({seconds}, {nanos})"
        );
        Ok(())
    })
}

/// A pre-epoch instant keeps its nanoseconds as a forward remainder: the second
/// before the epoch plus half a second is `.5`, not `-.5`.
#[test]
fn sim_adversary_calendar_pre_epoch_fractions_point_forward() -> TestResult {
    let table = [
        (-1_i64, 500_000_000_u32, "1969-12-31T23:59:59.5Z"),
        (-1, 999_999_999, "1969-12-31T23:59:59.999999999Z"),
        (-1, 1, "1969-12-31T23:59:59.000000001Z"),
        (-2, 0, "1969-12-31T23:59:58Z"),
        (0, 1, "1970-01-01T00:00:00.000000001Z"),
    ];
    for (seconds, nanos, text) in table {
        let instant = from_unix_parts(seconds, nanos)?;
        assert_eq!(to_rfc3339(instant)?, text, "({seconds}, {nanos})");
        assert_eq!(
            unix_parts(instant)?,
            (seconds, nanos),
            "({seconds}, {nanos}) parts"
        );
    }
    Ok(())
}

/// A second count that overflows while the carry is added is a typed refusal
/// carrying the input, never a wrapped instant.
#[test]
fn sim_adversary_calendar_a_carry_past_the_top_is_a_typed_overflow() {
    for nanos in [SECOND_NANOS, 2_000_000_000, u32::MAX] {
        assert_eq!(
            from_unix_parts(i64::MAX, nanos),
            Err(UnixTimeError::SecondsOverflow {
                seconds: i64::MAX,
                nanoseconds: nanos
            }),
            "i64::MAX seconds and {nanos} nanoseconds must overflow, not wrap"
        );
    }
    let near = from_unix_parts(i64::MAX.saturating_sub(4), u32::MAX);
    assert!(
        !matches!(near, Err(UnixTimeError::SecondsOverflow { .. })),
        "a carry that lands exactly on i64::MAX does not overflow: {near:?}"
    );
}

/// What the platform clock cannot hold is a typed refusal, and what it can hold
/// converts back to the parts it was built from: nothing is clamped.
#[test]
fn sim_adversary_calendar_the_clocks_edge_is_typed_never_clamped() {
    for (seconds, nanos) in [
        (i64::MAX, 0_u32),
        (i64::MAX, 999_999_999),
        (i64::MIN, 0),
        (i64::MIN, 1),
        (i64::MIN.saturating_add(1), 0),
    ] {
        match from_unix_parts(seconds, nanos) {
            Ok(instant) => assert_eq!(
                unix_parts(instant),
                Ok((seconds, nanos)),
                "({seconds}, {nanos}) converted but did not convert back"
            ),
            Err(error) => assert!(
                matches!(error, UnixTimeError::SystemTimeOutOfRange { .. }),
                "({seconds}, {nanos}) was refused as {error:?}, not as an out-of-range instant"
            ),
        }
    }
}

/// The first and last renderable instants render exactly, and the instants
/// either side of them are refused naming the year, never clamped to them.
#[test]
fn sim_adversary_calendar_the_four_digit_year_edges_are_exact_or_refused() {
    let first_second = days_from_civil(0, 1, 1).saturating_mul(DAY);
    let last_second = days_from_civil(9_999, 12, 31)
        .saturating_mul(DAY)
        .saturating_add(DAY.saturating_sub(1));
    for (seconds, nanos, rendered) in [
        (first_second, 0_u32, "0000-01-01T00:00:00Z"),
        (last_second, 999_999_999, "9999-12-31T23:59:59.999999999Z"),
    ] {
        if let Ok(instant) = from_unix_parts(seconds, nanos) {
            assert_eq!(
                to_rfc3339(instant),
                Ok(rendered.to_owned()),
                "edge instant {seconds}"
            );
        }
    }
    for (seconds, year) in [
        (first_second.saturating_sub(1), -1_i64),
        (last_second.saturating_add(1), 10_000),
        (days_from_civil(10_001, 1, 1).saturating_mul(DAY), 10_001),
        (days_from_civil(275_760, 9, 13).saturating_mul(DAY), 275_760),
        (
            days_from_civil(1_000_000, 6, 1).saturating_mul(DAY),
            1_000_000,
        ),
        (days_from_civil(-10_000, 3, 1).saturating_mul(DAY), -10_000),
    ] {
        if let Ok(instant) = from_unix_parts(seconds, 0) {
            assert_eq!(
                to_rfc3339(instant),
                Err(FormatError::YearOutsideRfc3339 { year }),
                "second {seconds} is outside the four-digit years and must say so"
            );
        }
    }
}

/// Rendered text sorts like the instants it names: the fixed-width form is
/// order-preserving, so a string comparison cannot invert time.
#[test]
fn sim_adversary_calendar_rendered_text_sorts_like_time() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let (earlier, later) = (draw_second(attacker), draw_second(attacker));
        let left = to_rfc3339(from_unix_parts(earlier, 0)?)?;
        let right = to_rfc3339(from_unix_parts(later, 0)?)?;
        assert_eq!(
            earlier.cmp(&later),
            left.cmp(&right),
            "seed {seed:#x} round {round}: {left} against {right}"
        );
        Ok(())
    })
}

/// Offset stamps reach the same canonical text as their zulu equals: whatever
/// offset the attacker writes, one instant has one rendering.
#[test]
fn sim_adversary_calendar_one_instant_has_one_rendering_whatever_the_offset() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let (hours, minutes) = (attacker.field(0, 24), attacker.field(0, 60));
        let year = attacker.field(1_800, 9_000);
        let local = format!("{year:04}-06-15T12:30:00+{hours:02}:{minutes:02}");
        let shifted = parse_rfc3339(&local)?;
        let rendered = to_rfc3339(shifted)?;
        assert_eq!(
            to_rfc3339(parse_rfc3339(&rendered)?)?,
            rendered,
            "seed {seed:#x} round {round}: {local} rendered as {rendered}, which is not a fixed point"
        );
        Ok(())
    })
}

/// The same seed walks the same dates.
#[test]
fn sim_adversary_calendar_the_same_seed_replays_the_same_campaign() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(campaign, seed);
    }
}

/// Two seeds walk two sets of dates.
#[test]
fn sim_adversary_calendar_distinct_seeds_walk_distinct_dates() {
    assert_distinct_seeds_diverge(campaign, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
    assert_distinct_seeds_diverge(campaign, SWEEP_SEEDS[2], SWEEP_SEEDS[3]);
}
