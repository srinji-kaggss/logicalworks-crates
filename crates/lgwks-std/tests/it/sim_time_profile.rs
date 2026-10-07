//! Public-contract regressions for the `SystemTime` RFC 3339 profile.

use std::time::{SystemTime, UNIX_EPOCH};

use lgwks_std::time::calendar::{civil_from_days, days_from_civil, try_days_in_month};

use crate::seeded_sweep;

use lgwks_std::time::format::{from_unix_parts, to_rfc3339, unix_parts};
use lgwks_std::time::{Field, FormatError, ParseError, UnixTimeError, parse_rfc3339};
use seeded_sweep::{assert_same_seed_replays, fold, initial_trace, next_seed};

use crate::seeded_bytes::next_byte;
use seeded_sweep::seeded_stream;

/// The length of `month` in `year`, or `refused` when the calendar refuses it.
///
/// The callers walk a date they have already checked, so the calendar's answer
/// is the one they advance by; `refused` is the degenerate length they use
/// instead, and it is the length that arm already treats as a one-day month.
fn month_length_or(year: i64, month: u32, refused: u32) -> u32 {
    match try_days_in_month(year, month) {
        Some(length) => length,
        None => refused,
    }
}

/// Counts a year length using the Gregorian divisibility rule directly.
fn reference_days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.rem_euclid(400) == 0
            || (year.rem_euclid(4) == 0 && year.rem_euclid(100) != 0) =>
        {
            29
        }
        2 => 28,
        _ => 0,
    }
}

/// Returns days before January 1 of an astronomical Gregorian year.
fn reference_days_before_year(year: i128) -> i128 {
    let prior_year = year.saturating_sub(1);
    year.saturating_mul(365)
        .saturating_add(prior_year.div_euclid(4))
        .saturating_sub(prior_year.div_euclid(100))
        .saturating_add(prior_year.div_euclid(400))
        .saturating_add(1)
}

/// Computes a valid civil date's day count with an independent year-length sum.
fn reference_days_from_civil(year: i64, month: u32, day: u32) -> i128 {
    let mut day_of_year = i128::from(day.saturating_sub(1));
    let mut current_month = 1u32;
    while current_month < month {
        let length = reference_days_in_month(year, current_month);
        day_of_year = day_of_year.saturating_add(i128::from(length));
        current_month = current_month.saturating_add(1);
    }
    reference_days_before_year(i128::from(year))
        .saturating_add(day_of_year)
        .saturating_sub(reference_days_before_year(1970))
}

/// Advances a valid civil date by one day.
fn next_date(year: i64, month: u32, day: u32) -> (i64, u32, u32) {
    // The caller walks from a date it has already checked, so the month length
    // is the one the calendar states; a month the calendar refuses is the one
    // day this advances to, which is the degenerate case the arm below already
    // handles as a one-day month.
    let month_length = month_length_or(year, month, 1);
    if day < month_length {
        (year, month, day.saturating_add(1))
    } else if month < 12 {
        (year, month.saturating_add(1), 1)
    } else {
        (year.saturating_add(1), 1, 1)
    }
}

/// Moves a valid civil date back by one day.
fn previous_date(year: i64, month: u32, day: u32) -> (i64, u32, u32) {
    if day > 1 {
        (year, month, day.saturating_sub(1))
    } else if month > 1 {
        let prior_month = month.saturating_sub(1);
        (year, prior_month, month_length_or(year, prior_month, 1))
    } else {
        let prior_year = year.saturating_sub(1);
        (prior_year, 12, month_length_or(prior_year, 12, 31))
    }
}

#[test]
/// Checks that invalid offset components report their exact field and byte.
fn invalid_numeric_offsets_name_the_field_and_byte() {
    let cases = [
        ("2000-01-01T00:00:00+24:00", Field::OffsetHour, 24, 23, 20),
        ("2000-01-01T00:00:00-24:00", Field::OffsetHour, 24, 23, 20),
        ("2000-01-01T00:00:00+00:60", Field::OffsetMinute, 60, 59, 23),
        ("2000-01-01T00:00:00-00:60", Field::OffsetMinute, 60, 59, 23),
        ("2000-01-01T00:00:00+99:99", Field::OffsetHour, 99, 23, 20),
    ];
    for (stamp, field, value, max, at) in cases {
        assert_eq!(
            parse_rfc3339(stamp),
            Err(ParseError::OutOfRange {
                field,
                value,
                min: 0,
                max,
                at,
            }),
            "invalid offset fields must fail at their original byte offset"
        );
    }
}

#[test]
/// Exercises RFC-compatible offset and calendar boundary controls.
fn valid_offsets_and_calendar_controls_remain_parseable() -> Result<(), ParseError> {
    let utc = parse_rfc3339("2000-02-29T00:00:00+00:00")?;
    assert_eq!(
        unix_parts(utc).map(|parts| parts.0),
        Ok(951_782_400),
        "UTC offset zero must preserve the leap-day date"
    );
    let local = parse_rfc3339("2000-02-29t23:59:59+23:59")?;
    assert_eq!(
        unix_parts(local).map(|parts| parts.0),
        Ok(951_782_459),
        "the maximum valid offset must normalize into the same UTC date"
    );
    let offset = parse_rfc3339("2023-11-15T00:13:20+02:00")?;
    assert_eq!(
        unix_parts(offset).map(|parts| parts.0),
        Ok(1_700_000_000),
        "numeric offsets must be subtracted from civil time"
    );
    Ok(())
}

#[test]
/// Ensures the SystemTime profile refuses leap seconds instead of rolling them.
fn leap_second_labels_are_explicitly_unsupported() {
    for stamp in [
        "2026-01-01T12:00:60Z",
        "2016-12-31T23:59:60Z",
        "2016-12-31T23:59:60+00:00",
    ] {
        assert_eq!(
            parse_rfc3339(stamp),
            Err(ParseError::UnsupportedLeapSecond { at: 17 }),
            "SystemTime parsing must never roll a leap-second label forward"
        );
    }
}

#[test]
/// Verifies fraction widths, exact nanoseconds, pre-epoch values, and trailing bytes.
fn fraction_width_and_pre_epoch_precision_are_exact() -> Result<(), ParseError> {
    let whole_seconds = parse_rfc3339("2023-11-14T22:13:20Z")?;
    assert_eq!(
        unix_parts(whole_seconds).map(|parts| parts.1),
        Ok(0),
        "an absent fraction must represent exactly zero nanoseconds"
    );
    assert_eq!(
        parse_rfc3339("2023-11-14T22:13:20.Z"),
        Err(ParseError::FractionWidth { digits: 0, at: 19 }),
        "a present fraction must contain at least one digit"
    );
    for (digits, expected) in [("1", 100_000_000), ("123456789", 123_456_789)] {
        let stamp = format!("1969-12-31T23:59:59.{digits}Z");
        let instant = parse_rfc3339(&stamp)?;
        assert_eq!(
            unix_parts(instant).map(|parts| parts.1),
            Ok(expected),
            "fractional digits must scale exactly to nanoseconds"
        );
        assert_eq!(
            to_rfc3339(instant),
            Ok(stamp),
            "pre-epoch fractions must render with the exact parsed digits"
        );
    }
    assert_eq!(
        parse_rfc3339("2023-11-14T22:13:20.1234567890Z"),
        Err(ParseError::FractionWidth { digits: 10, at: 19 }),
        "precision beyond nanoseconds must be refused without truncation"
    );
    assert_eq!(
        parse_rfc3339("2023-11-14T22:13:20Zx"),
        Err(ParseError::Malformed { at: 20, byte: b'x' }),
        "bytes after the UTC designator must be refused"
    );
    assert_eq!(
        parse_rfc3339("2023-11-14T22:13:20+00:00x"),
        Err(ParseError::Malformed { at: 25, byte: b'x' }),
        "bytes after a numeric offset must be refused at the suffix"
    );
    Ok(())
}

#[test]
/// Exercises checked conversion failures, carry normalization, and signed bounds.
fn checked_unix_conversion_never_substitutes_epoch() -> Result<(), UnixTimeError> {
    assert_eq!(
        from_unix_parts(0, 0)?,
        UNIX_EPOCH,
        "zero Unix parts must be the real epoch"
    );
    assert_eq!(
        from_unix_parts(i64::MAX, 1_000_000_000),
        Err(UnixTimeError::SecondsOverflow {
            seconds: i64::MAX,
            nanoseconds: 1_000_000_000,
        }),
        "overflow while normalizing nanos must remain distinguishable from epoch"
    );
    let carried = from_unix_parts(1, 1_500_000_000)?;
    assert_eq!(
        unix_parts(carried)?,
        (2, 500_000_000),
        "nanosecond overflow must normalize into seconds and remainder"
    );
    match from_unix_parts(i64::MIN, 0) {
        Ok(minimum) => assert_eq!(
            unix_parts(minimum),
            Ok((i64::MIN, 0)),
            "the exact lower signed endpoint must round-trip when supported"
        ),
        Err(UnixTimeError::SystemTimeOutOfRange { .. }) => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

#[test]
/// The checked conversions refuse exactly the two losses the deprecated lossy
/// endpoints used to take silently: a value past the platform's range is
/// refused rather than answered with the epoch, and an ordinary epoch reads back
/// exactly rather than as a default.
///
/// This is the evidence the deprecated spellings existed to warn about, stated
/// against the endpoints a caller is told to migrate to — and the deprecated
/// endpoints themselves are not called here, because a test that has to suppress
/// `deprecated` to reach the code it checks is no longer evidence about it.
fn checked_conversions_refuse_where_the_lossy_ones_fell_back() -> Result<(), UnixTimeError> {
    assert!(
        from_unix_parts(i64::MAX, 1_000_000_000).is_err(),
        "a conversion past the platform's range must be refused, not answered with the epoch"
    );
    assert_eq!(
        unix_parts(UNIX_EPOCH)?,
        (0, 0),
        "the checked extraction reads the epoch exactly"
    );
    Ok(())
}

#[test]
/// Compares public calendar conversions against an independent i128 reference.
fn calendar_roundtrips_endpoints_neighbors_and_overflow_transition() {
    let day_counts = [
        i64::MIN,
        i64::MIN.saturating_add(1),
        i64::MIN.saturating_add(2),
        i64::MAX.saturating_sub(2),
        i64::MAX.saturating_sub(1),
        i64::MAX,
    ];
    for days in day_counts {
        let (year, month, day) = civil_from_days(days);
        assert_eq!(
            reference_days_from_civil(year, month, day),
            i128::from(days),
            "wide independent reference must agree at day {days}"
        );
        assert_eq!(
            days_from_civil(year, month, day),
            days,
            "public calendar inverse must round-trip day {days}"
        );
    }

    for (year, month, day) in [
        (0, 1, 1),
        (4, 2, 29),
        (100, 3, 1),
        (400, 2, 29),
        (1600, 2, 29),
        (1900, 3, 1),
        (2000, 2, 29),
        (9999, 12, 31),
        (-400, 2, 29),
    ] {
        assert_eq!(
            i128::from(days_from_civil(year, month, day)),
            reference_days_from_civil(year, month, day),
            "wide reference must verify Gregorian boundary {year}-{month}-{day}"
        );
    }

    let (max_year, max_month, max_day) = civil_from_days(i64::MAX);
    let (next_year, next_month, next_day) = next_date(max_year, max_month, max_day);
    let beyond_max = reference_days_from_civil(next_year, next_month, next_day);
    assert_eq!(
        beyond_max,
        i128::from(i64::MAX).saturating_add(1),
        "the next civil date must be exactly one day beyond i64::MAX"
    );
    assert_eq!(
        days_from_civil(next_year, next_month, next_day),
        i64::MAX,
        "only the final narrowing may saturate above the upper endpoint"
    );

    let (min_year, min_month, min_day) = civil_from_days(i64::MIN);
    let (prior_year, prior_month, prior_day) = previous_date(min_year, min_month, min_day);
    let below_min = reference_days_from_civil(prior_year, prior_month, prior_day);
    assert_eq!(
        below_min,
        i128::from(i64::MIN).saturating_sub(1),
        "the prior civil date must be exactly one day below i64::MIN"
    );
    assert_eq!(
        days_from_civil(prior_year, prior_month, prior_day),
        i64::MIN,
        "only final narrowing may saturate below the lower endpoint"
    );
}

#[test]
/// Asserts the exact repaired values #153 T5 names at both signed endpoints.
///
/// The comment's arithmetic model reported `i64::MAX` mapping to
/// `(25252734927768524, 7, 27)` and returning `9223372036854056339` — an error
/// of `-719468` days caused by saturating before the epoch subtraction. These
/// are the repaired-SHA Rust results that item asks to be attached.
fn t5_endpoints_are_exact_under_the_repaired_narrowing() {
    // Upper endpoint: the civil date and its exact inverse.
    let (year, month, day) = civil_from_days(i64::MAX);
    assert_eq!(
        (year, month, day),
        (25_252_734_927_768_524, 7, 27),
        "the civil date at i64::MAX is fixed by the proleptic Gregorian calendar"
    );
    assert_eq!(
        days_from_civil(year, month, day),
        i64::MAX,
        "the upper endpoint must round-trip exactly, not 719468 days short"
    );

    // The endpoint's immediate neighbour must not collapse onto it, which is
    // exactly what the saturated model lost.
    let (neighbour_year, neighbour_month, neighbour_day) = civil_from_days(i64::MAX - 1);
    assert_eq!(
        (neighbour_year, neighbour_month, neighbour_day),
        (25_252_734_927_768_524, 7, 26),
        "i64::MAX-1 is the preceding day of the same month"
    );
    assert_eq!(
        days_from_civil(neighbour_year, neighbour_month, neighbour_day),
        i64::MAX - 1,
        "the neighbour must round-trip to its own value, not saturate to i64::MAX"
    );

    // Lower endpoint and its neighbour.
    let (min_year, min_month, min_day) = civil_from_days(i64::MIN);
    assert_eq!(
        days_from_civil(min_year, min_month, min_day),
        i64::MIN,
        "the lower endpoint must round-trip exactly"
    );
    let (below_year, below_month, below_day) = civil_from_days(i64::MIN + 1);
    assert_eq!(
        days_from_civil(below_year, below_month, below_day),
        i64::MIN + 1,
        "the lower neighbour must round-trip to its own value"
    );

    // Representative Gregorian-era and leap-day boundaries, against the
    // independent wide reference, both directions.
    for days in [
        0_i64,
        1,
        -1,
        -719_468,
        -719_469,
        719_468,
        146_096,
        146_097,
        365_242,
        i64::MIN.div_euclid(2),
        i64::MAX.div_euclid(2),
    ] {
        let (year, month, day) = civil_from_days(days);
        assert_eq!(
            reference_days_from_civil(year, month, day),
            i128::from(days),
            "the wide reference must agree at day {days}"
        );
        assert_eq!(
            days_from_civil(year, month, day),
            days,
            "the public inverse must round-trip day {days}"
        );
    }
}

#[test]
/// Checks the overflow-transition region against the wide reference.
///
/// Every value in a window around the point where the complete day count
/// leaves `i64` must either round-trip exactly or saturate to the declared
/// endpoint, and the declared endpoint must be the one reached.
fn t5_overflow_transition_region_is_exact_or_declared_saturated() {
    let boundary = i64::MAX;
    for offset in 0_i64..16 {
        let days = boundary.saturating_sub(offset);
        let (year, month, day) = civil_from_days(days);
        assert_eq!(
            days_from_civil(year, month, day),
            days,
            "inside the representable domain, day {days} must round-trip exactly"
        );
        let reference = reference_days_from_civil(year, month, day);
        assert_eq!(
            reference,
            i128::from(days),
            "the wide reference must agree at day {days}"
        );
    }

    // Beyond the endpoint the mathematical count is out of range, and only the
    // final narrowing may saturate — upwards, to exactly i64::MAX.
    for step in 1_u32..8 {
        let (year, month, day) = next_date(25_252_734_927_768_524, 7, 27_u32.saturating_add(step));
        let mathematical = reference_days_from_civil(year, month, day);
        assert!(
            mathematical > i128::from(boundary),
            "the probe date {year}-{month}-{day} must be beyond i64::MAX, got {mathematical}"
        );
        assert_eq!(
            days_from_civil(year, month, day),
            boundary,
            "only the final narrowing may saturate, and only upwards"
        );
    }

    // The same on the lower side.
    let (min_year, min_month, min_day) = civil_from_days(i64::MIN);
    let (prior_year, prior_month, prior_day) = previous_date(min_year, min_month, min_day);
    assert!(
        reference_days_from_civil(prior_year, prior_month, prior_day) < i128::from(i64::MIN),
        "the prior civil date must be below i64::MIN"
    );
    assert_eq!(
        days_from_civil(prior_year, prior_month, prior_day),
        i64::MIN,
        "only the final narrowing may saturate, and only downwards"
    );
}

#[test]
/// Exercises representative Gregorian-era and leap-day civil boundaries.
fn t5_gregorian_and_leap_day_boundaries_match_the_wide_reference() {
    for (year, month, day) in [
        (0_i64, 1_u32, 1_u32),
        (0, 2, 29),
        (1, 2, 28),
        (4, 2, 29),
        (100, 3, 1),
        (400, 2, 29),
        (1582, 10, 15),
        (1600, 2, 29),
        (1700, 3, 1),
        (1900, 3, 1),
        (1970, 1, 1),
        (2000, 2, 29),
        (9999, 12, 31),
        (10_000, 1, 1),
        (-1, 12, 31),
        (-4, 2, 29),
        (-100, 3, 1),
        (-400, 2, 29),
        (-401, 2, 28),
    ] {
        let expected = reference_days_from_civil(year, month, day);
        let observed = days_from_civil(year, month, day);
        // Every year here is inside the `i64` day domain except the largest
        // positive one, so the assertion states which case it is rather than
        // assuming.
        if expected >= i128::from(i64::MIN) && expected <= i128::from(i64::MAX) {
            assert_eq!(
                i128::from(observed),
                expected,
                "the wide reference must verify Gregorian boundary {year}-{month}-{day}"
            );
            let (round_trip_year, round_trip_month, round_trip_day) = civil_from_days(observed);
            assert_eq!(
                (round_trip_year, round_trip_month, round_trip_day),
                (year, month, day),
                "{year}-{month}-{day} must round-trip through the public inverse"
            );
        } else {
            assert_eq!(
                observed,
                i64::MAX,
                "a day count beyond i64::MAX must saturate to the declared endpoint"
            );
        }
    }
}

/// Runs seeded day and civil-date samples and returns their deterministic trace.
fn run_seeded_calendar_samples(seed: u64) -> u64 {
    let mut state = seeded_stream(seed);
    let mut trace = initial_trace();
    for _ in 0..10_000 {
        let day_seed = next_seed(&mut state);
        let days = i64::from_le_bytes(day_seed.to_le_bytes());
        let (year, month, day) = civil_from_days(days);
        assert_eq!(
            reference_days_from_civil(year, month, day),
            i128::from(days),
            "seeded day sample must match the independent wide reference"
        );
        assert_eq!(
            days_from_civil(year, month, day),
            days,
            "seeded day sample must round-trip through the public inverse"
        );
        fold(&mut trace, u64::from_le_bytes(days.to_le_bytes()));

        // The civil date is drawn in the widths the calendar takes it in: the
        // year from two bytes of one stream word, the month from a byte, and the
        // day from a byte reduced by that month's length. Two bytes carry 65 536
        // years around the epoch, which spans leap rules, century rules and
        // whole 400-year cycles — the calendar's own periods — without a
        // conversion between the stream's width and the year field's.
        let word = next_seed(&mut state).to_le_bytes();
        let year = i64::from(u16::from_le_bytes([word[0], word[1]])).saturating_sub(32_768);
        let month = u32::from(next_byte(&mut state))
            .rem_euclid(12)
            .saturating_add(1);
        let month_days = month_length_or(year, month, 31);
        let day = u32::from(next_byte(&mut state))
            .rem_euclid(month_days)
            .saturating_add(1);
        let expected = reference_days_from_civil(year, month, day);
        let actual_days = days_from_civil(year, month, day);
        assert_eq!(
            i128::from(actual_days),
            expected,
            "seeded valid civil date must match the independent wide reference"
        );
        let (roundtrip_year, roundtrip_month, roundtrip_day) = civil_from_days(actual_days);
        assert_eq!(
            (roundtrip_year, roundtrip_month, roundtrip_day),
            (year, month, day),
            "seeded valid civil date must round-trip through public calendar APIs"
        );
        // The year is a signed calendar field, so it is folded as its own
        // two's-complement bits rather than narrowed: a negative year and a
        // positive one that shares its low bits must not fold alike.
        fold(&mut trace, year.cast_unsigned());
        fold(&mut trace, u64::from(month));
        fold(&mut trace, u64::from(day));
    }
    trace
}

#[test]
/// Replays 10,000 seeded day and civil-date pairs through the public API.
fn seeded_calendar_model_replays_exactly() {
    assert_same_seed_replays(run_seeded_calendar_samples, 0x1530_2026_0928);
}

#[test]
/// Uses the public parse and format surface like a downstream consumer.
fn canonical_external_parse_and_format_surface_is_checked() -> Result<(), Box<dyn std::error::Error>>
{
    let instant = from_unix_parts(-1, 500_000_000)?;
    let stamp = to_rfc3339(instant)?;
    assert_eq!(
        stamp, "1969-12-31T23:59:59.5Z",
        "the canonical public formatter must preserve pre-epoch fractions"
    );
    let parsed: SystemTime = parse_rfc3339(&stamp)?;
    assert_eq!(
        unix_parts(parsed)?,
        (-1, 500_000_000),
        "the public parser must recover the exact Unix parts"
    );
    assert_eq!(
        to_rfc3339(parsed)?,
        stamp,
        "the public parse and format journey must round-trip"
    );
    Ok(())
}

#[test]
/// Checks four-digit UTC output and offset normalization at year boundaries.
fn rfc_year_boundaries_refuse_extended_output_without_clamping()
-> Result<(), Box<dyn std::error::Error>> {
    let lower_secs = days_from_civil(0, 1, 1).saturating_mul(86_400);
    match from_unix_parts(lower_secs, 0) {
        Ok(instant) => assert_eq!(
            to_rfc3339(instant),
            Ok("0000-01-01T00:00:00Z".to_owned()),
            "year zero must use exactly four digits"
        ),
        Err(UnixTimeError::SystemTimeOutOfRange { .. }) => {}
        Err(error) => return Err(Box::new(error)),
    }

    let year_10000_secs = days_from_civil(10_000, 1, 1).saturating_mul(86_400);
    match from_unix_parts(year_10000_secs, 0) {
        Ok(instant) => assert_eq!(
            to_rfc3339(instant),
            Err(FormatError::YearOutsideRfc3339 { year: 10_000 }),
            "year 10000 must be rejected instead of emitted as extended RFC text"
        ),
        Err(UnixTimeError::SystemTimeOutOfRange { .. }) => {}
        Err(error) => return Err(Box::new(error)),
    }

    let before_year_zero = parse_rfc3339("0000-01-01T00:00:00+00:01");
    match before_year_zero {
        Ok(instant) => assert_eq!(
            to_rfc3339(instant),
            Err(FormatError::YearOutsideRfc3339 { year: -1 }),
            "UTC normalization before year zero must not format as RFC 3339"
        ),
        Err(ParseError::UnrepresentableInstant(UnixTimeError::SystemTimeOutOfRange { .. })) => {}
        Err(error) => return Err(Box::new(error)),
    }

    let after_year_9999 = parse_rfc3339("9999-12-31T23:59:59-00:01");
    match after_year_9999 {
        Ok(instant) => assert_eq!(
            to_rfc3339(instant),
            Err(FormatError::YearOutsideRfc3339 { year: 10_000 }),
            "UTC normalization beyond year 9999 must be refused"
        ),
        Err(ParseError::UnrepresentableInstant(UnixTimeError::SystemTimeOutOfRange { .. })) => {}
        Err(error) => return Err(Box::new(error)),
    }
    Ok(())
}
