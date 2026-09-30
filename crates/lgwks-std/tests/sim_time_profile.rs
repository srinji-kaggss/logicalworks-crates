//! Public-contract regressions for the `SystemTime` RFC 3339 profile.

use std::time::{SystemTime, UNIX_EPOCH};

use lgwks_std::time::calendar::{civil_from_days, days_from_civil, try_days_in_month};
use lgwks_std::time::format::{from_unix_parts, to_rfc3339, unix_parts};
use lgwks_std::time::{Field, FormatError, ParseError, UnixTimeError, parse_rfc3339};

/// Calls deprecated lossy endpoints from this downstream integration crate.
#[expect(
    deprecated,
    reason = "exercise the explicitly lossy migration endpoints as a downstream caller"
)]
fn lossy_compatibility_values() -> (SystemTime, (i64, u32)) {
    (
        lgwks_std::time::format::from_unix_parts_lossy(i64::MAX, 1_000_000_000),
        lgwks_std::time::format::unix_parts_lossy(UNIX_EPOCH),
    )
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
    let month_length = try_days_in_month(year, month).unwrap_or(1);
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
        (
            year,
            prior_month,
            try_days_in_month(year, prior_month).unwrap_or(1),
        )
    } else {
        let prior_year = year.saturating_sub(1);
        (
            prior_year,
            12,
            try_days_in_month(prior_year, 12).unwrap_or(31),
        )
    }
}

/// Advances a deterministic xorshift-style test stream.
fn next_seed(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    *state
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
/// Verifies legacy loss is reachable only through explicitly named APIs.
fn lossy_compatibility_entry_points_are_explicit() {
    let (epoch_fallback, epoch_parts) = lossy_compatibility_values();
    assert_eq!(
        epoch_fallback, UNIX_EPOCH,
        "the explicitly lossy conversion keeps its documented epoch fallback"
    );
    assert_eq!(
        epoch_parts,
        (0, 0),
        "the explicitly lossy extraction preserves ordinary epoch values"
    );
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

/// Runs seeded day and civil-date samples and returns their deterministic trace.
fn run_seeded_calendar_samples(seed: u64) -> u64 {
    let mut state = seed;
    let mut trace = 14_695_981_039_346_656_037u64;
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
        trace = trace
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(u64::from_le_bytes(days.to_le_bytes()));

        let year = i64::try_from(next_seed(&mut state).rem_euclid(800_001))
            .unwrap_or(0)
            .saturating_sub(400_000);
        let month = u32::try_from(next_seed(&mut state).rem_euclid(12))
            .unwrap_or(0)
            .saturating_add(1);
        let month_days = try_days_in_month(year, month).unwrap_or(31);
        let day = u32::try_from(next_seed(&mut state).rem_euclid(u64::from(month_days)))
            .unwrap_or(0)
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
        trace = trace
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(u64::try_from(year).unwrap_or(0))
            .wrapping_add(u64::from(month))
            .wrapping_add(u64::from(day));
    }
    trace
}

#[test]
/// Replays 10,000 seeded day and civil-date pairs through the public API.
fn seeded_calendar_model_replays_exactly() {
    let first_trace = run_seeded_calendar_samples(0x1530_2026_0928);
    let replayed_trace = run_seeded_calendar_samples(0x1530_2026_0928);
    assert_eq!(
        first_trace, replayed_trace,
        "the same seed must produce the exact same calendar trace"
    );
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
