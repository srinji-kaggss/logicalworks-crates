//! RFC 3339 string parsing routines.
//!
//! Enforces ISO 8601 / RFC 3339 compliance without external dependencies.
//! Validates leap years, calendar bounds, 24-hour time ranges, nanosecond
//! precision, and UTC / numeric offsets.

use std::time::SystemTime;

use super::calendar::{days_from_civil, days_in_month};
use super::error::{Field, ParseError};
use super::format::from_unix_parts;

/// Validates that the input buffer satisfies the minimum RFC 3339 length.
fn check_min_len(bytes: &[u8]) -> Result<(), ParseError> {
    if bytes.len() < 19 {
        Err(ParseError::TooShort {
            len: bytes.len(),
            at: bytes.len(),
        })
    } else {
        Ok(())
    }
}

/// Asserts that the byte at `at` matches the expected delimiter character.
fn expect_byte(bytes: &[u8], at: usize, expected: u8) -> Result<(), ParseError> {
    match bytes.get(at) {
        Some(&actual) if actual == expected => Ok(()),
        Some(&actual) => Err(ParseError::Malformed { at, byte: actual }),
        None => Err(ParseError::TooShort {
            len: bytes.len(),
            at,
        }),
    }
}

/// Converts an ASCII digit byte to its numeric value.
fn check_digit(byte: u8, target_field: Field, at: usize) -> Result<u32, ParseError> {
    if byte.is_ascii_digit() {
        Ok(u32::from(byte - b'0'))
    } else {
        Err(ParseError::NonDigit {
            field: target_field,
            at,
            byte,
        })
    }
}

/// Parses fixed-width ASCII digit sequences into an unsigned integer.
fn parse_digit_field(
    bytes: &[u8],
    start: usize,
    len: usize,
    target_field: Field,
) -> Result<u32, ParseError> {
    let mut acc = 0u32;
    for offset in 0..len {
        let at = start + offset;
        let byte = *bytes.get(at).ok_or(ParseError::TooShort {
            len: bytes.len(),
            at,
        })?;
        let digit = check_digit(byte, target_field, at)?;
        acc = acc * 10 + digit;
    }
    Ok(acc)
}

/// Enforces that a parsed value falls within the valid inclusive range.
fn check_range(field: Field, value: u32, min: u32, max: u32, at: usize) -> Result<(), ParseError> {
    if value < min || value > max {
        Err(ParseError::OutOfRange {
            field,
            value,
            min,
            max,
            at,
        })
    } else {
        Ok(())
    }
}

/// Parses the 4-digit Gregorian year.
fn parse_year(bytes: &[u8]) -> Result<i64, ParseError> {
    let year_val = parse_digit_field(bytes, 0, 4, Field::Year)?;
    expect_byte(bytes, 4, b'-')?;
    Ok(i64::from(year_val))
}

/// Parses the 2-digit month (1..=12).
fn parse_month(bytes: &[u8]) -> Result<u32, ParseError> {
    let month_val = parse_digit_field(bytes, 5, 2, Field::Month)?;
    expect_byte(bytes, 7, b'-')?;
    check_range(Field::Month, month_val, 1, 12, 5)?;
    Ok(month_val)
}

/// Parses the 2-digit day of month for the given year and month.
fn parse_day(bytes: &[u8], year: i64, month: u32) -> Result<u32, ParseError> {
    let day_val = parse_digit_field(bytes, 8, 2, Field::Day)?;
    check_range(Field::Day, day_val, 1, days_in_month(year, month), 8)?;
    Ok(day_val)
}

/// Parses the date components: `YYYY-MM-DD`.
fn parse_date(bytes: &[u8]) -> Result<(i64, u32, u32), ParseError> {
    let year = parse_year(bytes)?;
    let month = parse_month(bytes)?;
    let day = parse_day(bytes, year, month)?;
    Ok((year, month, day))
}

/// Validates the date-time separator character (`T`, `t`, or space).
fn check_time_separator(bytes: &[u8]) -> Result<(), ParseError> {
    match bytes.get(10) {
        Some(b'T' | b't' | b' ') => Ok(()),
        Some(&byte) => Err(ParseError::Malformed { at: 10, byte }),
        None => Err(ParseError::TooShort {
            len: bytes.len(),
            at: 10,
        }),
    }
}

/// Parses the 2-digit hour component (0..=23).
fn parse_hour(bytes: &[u8]) -> Result<u32, ParseError> {
    let hour_val = parse_digit_field(bytes, 11, 2, Field::Hour)?;
    expect_byte(bytes, 13, b':')?;
    check_range(Field::Hour, hour_val, 0, 23, 11)?;
    Ok(hour_val)
}

/// Parses the 2-digit minute component (0..=59).
fn parse_minute(bytes: &[u8]) -> Result<u32, ParseError> {
    let min_val = parse_digit_field(bytes, 14, 2, Field::Minute)?;
    expect_byte(bytes, 16, b':')?;
    check_range(Field::Minute, min_val, 0, 59, 14)?;
    Ok(min_val)
}

/// Parses the 2-digit second component (0..=60, including leap second).
fn parse_second(bytes: &[u8]) -> Result<u32, ParseError> {
    let sec_val = parse_digit_field(bytes, 17, 2, Field::Second)?;
    check_range(Field::Second, sec_val, 0, 60, 17)?;
    Ok(sec_val)
}

/// Parses the complete `HH:MM:SS` time components.
fn parse_time(bytes: &[u8]) -> Result<(u32, u32, u32), ParseError> {
    check_time_separator(bytes)?;
    let hour = parse_hour(bytes)?;
    let minute = parse_minute(bytes)?;
    let second = parse_second(bytes)?;
    Ok((hour, minute, second))
}

/// Computes scaled nanoseconds from variable fractional digit slice.
fn compute_fraction(bytes: &[u8], start: usize, digits: usize) -> u32 {
    let mut scaled = 0u32;
    for digit_idx in 0..9 {
        scaled = scaled * 10
            + if digit_idx < digits {
                u32::from(bytes[start + digit_idx] - b'0')
            } else {
                0
            };
    }
    scaled
}

/// Collects and scales fractional digits after the decimal dot.
fn parse_fraction_digits(
    bytes: &[u8],
    cursor: &mut usize,
    dot_pos: usize,
) -> Result<u32, ParseError> {
    let start = *cursor;
    while *cursor < bytes.len() && bytes[*cursor].is_ascii_digit() {
        *cursor += 1;
    }
    let digits = *cursor - start;
    if digits == 0 || digits > 9 {
        Err(ParseError::FractionWidth {
            digits,
            at: dot_pos,
        })
    } else {
        Ok(compute_fraction(bytes, start, digits))
    }
}

/// Parses optional fractional nanoseconds if present.
fn parse_fraction(bytes: &[u8], cursor: &mut usize) -> Result<u32, ParseError> {
    if bytes.get(*cursor) != Some(&b'.') {
        return Ok(0);
    }
    let dot_pos = *cursor;
    *cursor += 1;
    parse_fraction_digits(bytes, cursor, dot_pos)
}

/// Parses a fixed-width `±HH:MM` numeric timezone offset into signed minutes.
fn parse_numeric_offset(
    bytes: &[u8],
    cursor: usize,
    sign_negative: bool,
) -> Result<i64, ParseError> {
    if cursor + 6 != bytes.len() {
        Err(ParseError::MissingOffset { at: cursor })
    } else {
        let offset_hour = parse_digit_field(bytes, cursor + 1, 2, Field::OffsetHour)?;
        expect_byte(bytes, cursor + 3, b':')?;
        let offset_minute = parse_digit_field(bytes, cursor + 4, 2, Field::OffsetMinute)?;
        let magnitude = i64::from(offset_hour) * 60 + i64::from(offset_minute);
        if sign_negative {
            Ok(-magnitude)
        } else {
            Ok(magnitude)
        }
    }
}

/// Parses timezone offset (`Z` or `±HH:MM`).
fn parse_offset(bytes: &[u8], cursor: usize) -> Result<i64, ParseError> {
    match bytes.get(cursor) {
        None => Err(ParseError::MissingOffset { at: cursor }),
        Some(b'Z' | b'z') if cursor + 1 == bytes.len() => Ok(0),
        Some(b'+') => parse_numeric_offset(bytes, cursor, false),
        Some(b'-') => parse_numeric_offset(bytes, cursor, true),
        Some(&byte) => Err(ParseError::Malformed { at: cursor, byte }),
    }
}

/// Parses RFC 3339 formatted text into a [`SystemTime`].
pub fn parse_rfc3339(text: &str) -> Result<SystemTime, ParseError> {
    let bytes = text.as_bytes();
    check_min_len(bytes)?;
    let (year, month, day) = parse_date(bytes)?;
    let (hour, minute, second) = parse_time(bytes)?;
    let mut cursor = 19;
    let nanos = parse_fraction(bytes, &mut cursor)?;
    let offset_minutes = parse_offset(bytes, cursor)?;

    let secs = days_from_civil(year, month, day) * 86_400
        + i64::from(hour) * 3600
        + i64::from(minute) * 60
        + i64::from(second)
        - offset_minutes * 60;
    Ok(from_unix_parts(secs, nanos))
}
