//! `time` owns RFC 3339 formatting and parsing over `std::time::SystemTime`,
//! enforcing INV-TIME-PURE: all formatting and parsing is pure, timezone-agnostic
//! (UTC-normalized), allocation-minimal, and requires zero external dependencies
//! like `chrono`.

pub mod calendar;
pub mod error;
pub mod format;
pub mod parse;

pub use calendar::{civil_from_days, days_from_civil, days_in_month, is_leap};
pub use error::{Field, ParseError};
pub use format::{from_unix_parts, to_rfc3339, unix_parts};
pub use parse::parse_rfc3339;

use std::time::SystemTime;

/// Formats the current instant as an RFC 3339 UTC string.
pub fn now_rfc3339() -> String {
    to_rfc3339(SystemTime::now())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn at(secs: i64) -> SystemTime {
        from_unix_parts(secs, 0)
    }

    #[test]
    fn epoch_day_zero_is_nineteen_seventy() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn civil_roundtrips_across_four_centuries() {
        let mut days = -719_468;
        while days < 719_468 * 2 {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(
                days_from_civil(year, month, day),
                days,
                "failed at day {days}"
            );
            days += 97;
        }
    }

    #[test]
    fn epoch_renders_without_a_fraction() {
        assert_eq!(to_rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn nanoseconds_render_at_full_precision() {
        let instant = UNIX_EPOCH + Duration::new(0, 123_456_789);
        assert_eq!(to_rfc3339(instant), "1970-01-01T00:00:00.123456789Z");
    }

    #[test]
    fn short_fractions_are_left_aligned() {
        let instant = UNIX_EPOCH + Duration::new(0, 120_000_000);
        assert_eq!(to_rfc3339(instant), "1970-01-01T00:00:00.12Z");
    }

    #[test]
    fn a_known_instant_renders_exactly() {
        let instant = UNIX_EPOCH + Duration::new(1_700_000_000, 500_000_000);
        assert_eq!(to_rfc3339(instant), "2023-11-14T22:13:20.5Z");
    }

    #[test]
    fn parse_reverses_render_for_the_current_instant() {
        let original = SystemTime::now();
        let rendered = to_rfc3339(original);
        let parsed = parse_rfc3339(&rendered).expect("roundtrips");
        assert_eq!(to_rfc3339(parsed), rendered);
    }

    #[test]
    fn lowercase_separator_and_zulu_are_accepted() {
        let parsed = parse_rfc3339("2023-11-14t22:13:20z").expect("parses lowercase");
        assert_eq!(parsed, at(1_700_000_000));
    }

    #[test]
    fn a_numeric_offset_is_folded_into_utc() {
        let parsed = parse_rfc3339("2023-11-15T00:13:20+02:00").expect("parses +02:00");
        assert_eq!(parsed, at(1_700_000_000));
    }

    #[test]
    fn a_missing_offset_is_refused() {
        assert_eq!(
            parse_rfc3339("2023-11-14T22:13:20"),
            Err(ParseError::MissingOffset { at: 19 })
        );
    }

    #[test]
    fn a_non_numeric_field_names_itself() {
        assert_eq!(
            parse_rfc3339("2023-XX-14T22:13:20Z"),
            Err(ParseError::NonDigit {
                field: Field::Month,
                at: 5,
                byte: b'X',
            })
        );
    }

    #[test]
    fn an_out_of_range_month_is_refused() {
        assert_eq!(
            parse_rfc3339("2023-13-14T22:13:20Z"),
            Err(ParseError::OutOfRange {
                field: Field::Month,
                value: 13,
                min: 1,
                max: 12,
                at: 5,
            })
        );
    }

    #[test]
    fn leap_day_parses_only_in_a_leap_year() {
        assert!(parse_rfc3339("2024-02-29T12:00:00Z").is_ok());
        assert!(matches!(
            parse_rfc3339("2023-02-29T12:00:00Z"),
            Err(ParseError::OutOfRange {
                field: Field::Day,
                ..
            })
        ));
    }

    #[test]
    fn an_over_wide_fraction_is_refused() {
        assert_eq!(
            parse_rfc3339("2023-11-14T22:13:20.1234567890Z"),
            Err(ParseError::FractionWidth { digits: 10, at: 19 })
        );
    }

    #[test]
    fn pre_epoch_instants_are_preserved_not_clamped() {
        let stamp = "1969-12-31T23:59:59Z";
        let parsed = parse_rfc3339(stamp).expect("parses pre-epoch");
        assert_eq!(to_rfc3339(parsed), stamp);
    }

    #[test]
    fn pre_epoch_fractions_round_trip() {
        let stamp = "1969-12-31T23:59:59.5Z";
        let parsed = parse_rfc3339(stamp).expect("parses pre-epoch fraction");
        assert_eq!(to_rfc3339(parsed), stamp);
    }

    #[test]
    fn the_instant_type_is_std_system_time() {
        let parsed: SystemTime = parse_rfc3339("2026-08-17T00:00:00Z").unwrap();
        assert!(parsed > UNIX_EPOCH);
    }

    #[test]
    fn now_is_after_the_start_of_twenty_twenty_six() {
        let now = SystemTime::now();
        let twenty_twenty_six = parse_rfc3339("2026-01-01T00:00:00Z").unwrap();
        assert!(now > twenty_twenty_six);
    }
}
