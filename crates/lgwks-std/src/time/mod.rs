//! `time` owns RFC 3339 formatting and parsing over `std::time::SystemTime`,
//! enforcing INV-TIME-PURE: all formatting and parsing is pure, timezone-agnostic
//! (UTC-normalized), allocation-minimal, and requires zero external dependencies
//! like `chrono`.

pub mod calendar;
pub mod error;
pub mod format;
pub mod parse;

// `calendar` and `format` are public modules, so their items are reached as
// `time::format::to_rfc3339`. Re-exporting them here as well made two names for
// one thing, and nothing in the workspace used the short one. What stays is the
// module's own pair — [`now_rfc3339`] and `parse_rfc3339`, format and parse —
// plus the error `parse` returns, so a caller matching on that failure names it
// without having to know which submodule defined it. `time::parse::parse_rfc3339`
// would stutter, and the low-level calendar arithmetic is what the submodules
// are for.
pub use error::{Field, ParseError};
pub use parse::parse_rfc3339;

use std::time::SystemTime;

/// Formats the current instant as an RFC 3339 UTC string.
#[must_use]
pub fn now_rfc3339() -> String {
    format::to_rfc3339(SystemTime::now())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    // The low-level halves are reached by their own path now, so these tests
    // name the submodule they come from rather than leaning on a re-export.
    use crate::time::calendar::{civil_from_days, days_from_civil};
    use crate::time::format::{from_unix_parts, to_rfc3339};
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

    // Tests that parse return `Result`: a parse refusal reports its own `Debug`
    // on failure, which is the same report `.expect` would have panicked with,
    // without an `expect` in the tree.
    #[test]
    fn parse_reverses_render_for_the_current_instant() -> Result<(), ParseError> {
        let original = SystemTime::now();
        let rendered = to_rfc3339(original);
        let parsed = parse_rfc3339(&rendered)?;
        assert_eq!(to_rfc3339(parsed), rendered);
        Ok(())
    }

    #[test]
    fn lowercase_separator_and_zulu_are_accepted() -> Result<(), ParseError> {
        let parsed = parse_rfc3339("2023-11-14t22:13:20z")?;
        assert_eq!(parsed, at(1_700_000_000));
        Ok(())
    }

    #[test]
    fn a_numeric_offset_is_folded_into_utc() -> Result<(), ParseError> {
        let parsed = parse_rfc3339("2023-11-15T00:13:20+02:00")?;
        assert_eq!(parsed, at(1_700_000_000));
        Ok(())
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
    fn pre_epoch_instants_are_preserved_not_clamped() -> Result<(), ParseError> {
        let stamp = "1969-12-31T23:59:59Z";
        let parsed = parse_rfc3339(stamp)?;
        assert_eq!(to_rfc3339(parsed), stamp);
        Ok(())
    }

    #[test]
    fn pre_epoch_fractions_round_trip() -> Result<(), ParseError> {
        let stamp = "1969-12-31T23:59:59.5Z";
        let parsed = parse_rfc3339(stamp)?;
        assert_eq!(to_rfc3339(parsed), stamp);
        Ok(())
    }

    #[test]
    fn the_instant_type_is_std_system_time() -> Result<(), ParseError> {
        let parsed: SystemTime = parse_rfc3339("2026-08-17T00:00:00Z")?;
        assert!(parsed > UNIX_EPOCH);
        Ok(())
    }

    #[test]
    fn now_is_after_the_start_of_twenty_twenty_six() -> Result<(), ParseError> {
        let now = SystemTime::now();
        let twenty_twenty_six = parse_rfc3339("2026-01-01T00:00:00Z")?;
        assert!(now > twenty_twenty_six);
        Ok(())
    }
}
