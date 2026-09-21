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
///
/// The caller has already established `byte.is_ascii_digit()`, so `byte - b'0'`
/// is in `0..=9`; `saturating_sub` states that bound rather than relying on it.
fn check_digit(byte: u8, target_field: Field, at: usize) -> Result<u32, ParseError> {
    if byte.is_ascii_digit() {
        Ok(u32::from(byte.saturating_sub(b'0')))
    } else {
        Err(ParseError::NonDigit {
            field: target_field,
            at,
            byte,
        })
    }
}

/// Parses fixed-width ASCII digit sequences into an unsigned integer.
///
/// `len` is 2 or 4 at every call site and never exceeds the caller's bounds, so
/// `start + offset` stays inside the input: both operands are bounded by
/// `bytes.len()`, which no slice index can exceed. At most four digits are
/// accumulated, so `acc` tops out at `9_999` and neither `saturating_*` below
/// can saturate.
fn parse_digit_field(
    bytes: &[u8],
    start: usize,
    len: usize,
    target_field: Field,
) -> Result<u32, ParseError> {
    let mut acc = 0u32;
    for offset in 0..len {
        let at = start.saturating_add(offset);
        let byte = *bytes.get(at).ok_or(ParseError::TooShort {
            len: bytes.len(),
            at,
        })?;
        let digit = check_digit(byte, target_field, at)?;
        acc = acc.saturating_mul(10).saturating_add(digit);
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
///
/// RFC 3339 makes `T` the canonical separator and permits a lower-case `t` or a
/// space in its place; all three are accepted here.
fn check_time_separator(bytes: &[u8]) -> Result<(), ParseError> {
    // `copied()` puts the match on `u8` rather than `&u8`, so every arm binds a
    // value and the byte patterns below read as written.
    match bytes.get(10).copied() {
        Some(b'T' | b't' | b' ') => Ok(()),
        Some(byte) => Err(ParseError::Malformed { at: 10, byte }),
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
///
/// `digits` is `1..=9`. Position `digit_idx` is read only while
/// `digit_idx < digits`, so every index is inside the fractional run the caller
/// measured; a byte that is not there contributes `0`, which is exactly what
/// left-aligning a short fraction means (`0.12` is `120_000_000` ns). Nine
/// digits scale to at most `999_999_999`, so neither `saturating_*` below can
/// saturate.
fn compute_fraction(bytes: &[u8], start: usize, digits: usize) -> u32 {
    let mut scaled = 0u32;
    for digit_idx in 0..9 {
        let digit = if digit_idx < digits {
            let byte = bytes
                .get(start.saturating_add(digit_idx))
                .copied()
                .unwrap_or(b'0');
            u32::from(byte.saturating_sub(b'0'))
        } else {
            0
        };
        scaled = scaled.saturating_mul(10).saturating_add(digit);
    }
    scaled
}

/// Collects and scales fractional digits after the decimal dot.
///
/// Advances `cursor` past the run of digits and reports the run's width, which
/// must be `1..=9` — RFC 3339 permits truncation, but not an empty fraction nor
/// more precision than a nanosecond.
fn parse_fraction_digits(
    bytes: &[u8],
    cursor: &mut usize,
    dot_pos: usize,
) -> Result<u32, ParseError> {
    let start = *cursor;
    while *cursor < bytes.len() && bytes[*cursor].is_ascii_digit() {
        // `*cursor < bytes.len()` on entry, so this cannot saturate.
        *cursor = cursor.saturating_add(1);
    }
    // The cursor only ever advances, so `*cursor >= start`; both are bounded by
    // `bytes.len()`.
    let digits = cursor.saturating_sub(start);
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
///
/// A missing fraction is `0` nanoseconds rather than an error, because RFC 3339
/// makes the fractional part optional; a present but empty or over-wide one is
/// refused by [`parse_fraction_digits`].
fn parse_fraction(bytes: &[u8], cursor: &mut usize) -> Result<u32, ParseError> {
    if bytes.get(*cursor) != Some(&b'.') {
        return Ok(0);
    }
    let dot_pos = *cursor;
    // `*cursor` indexes a `.` inside the input, so the step stays in bounds.
    *cursor = cursor.saturating_add(1);
    parse_fraction_digits(bytes, cursor, dot_pos)
}

/// Parses a fixed-width `±HH:MM` numeric timezone offset into signed minutes.
///
/// The caller has established that `cursor` indexes a `+` or `-`. The offset is
/// `6` bytes wide, so the check below is also what proves the four inner
/// offsets stay in bounds; each is at most `4`, and `cursor + 4 <= bytes.len()`
/// once the check passes, so none of the `saturating_*` calls can saturate.
fn parse_numeric_offset(
    bytes: &[u8],
    cursor: usize,
    sign_negative: bool,
) -> Result<i64, ParseError> {
    if cursor.saturating_add(6) != bytes.len() {
        Err(ParseError::MissingOffset { at: cursor })
    } else {
        let offset_hour = parse_digit_field(bytes, cursor.saturating_add(1), 2, Field::OffsetHour)?;
        expect_byte(bytes, cursor.saturating_add(3), b':')?;
        let offset_minute =
            parse_digit_field(bytes, cursor.saturating_add(4), 2, Field::OffsetMinute)?;
        // Both fields are two digits, so the magnitude is at most
        // `99 * 60 + 99` and neither operation can saturate.
        let magnitude = i64::from(offset_hour)
            .saturating_mul(60)
            .saturating_add(i64::from(offset_minute));
        if sign_negative {
            Ok(magnitude.saturating_neg())
        } else {
            Ok(magnitude)
        }
    }
}

/// Parses timezone offset (`Z` or `±HH:MM`) into signed minutes from UTC.
///
/// `Z` (and its lower-case form) is only accepted as the last byte, so trailing
/// junk after a zulu designator is a [`ParseError::Malformed`] rather than a
/// silent success.
fn parse_offset(bytes: &[u8], cursor: usize) -> Result<i64, ParseError> {
    // `copied()` puts the match on `u8` rather than `&u8`, so the byte patterns
    // below read as written instead of as `&b'Z'` alternatives.
    match bytes.get(cursor).copied() {
        None => Err(ParseError::MissingOffset { at: cursor }),
        Some(b'Z' | b'z') if cursor.saturating_add(1) == bytes.len() => Ok(0),
        Some(b'+') => parse_numeric_offset(bytes, cursor, false),
        Some(b'-') => parse_numeric_offset(bytes, cursor, true),
        Some(byte) => Err(ParseError::Malformed { at: cursor, byte }),
    }
}

/// Parses RFC 3339 formatted text into a [`SystemTime`].
///
/// The instant is normalised to UTC: a numeric `±HH:MM` offset is subtracted
/// from the civil time rather than stored, so two stamps naming the same
/// instant compare equal whatever offset they were written with.
///
/// # Errors
///
/// Returns [`ParseError`] naming the first field or separator that failed, with
/// the byte offset it failed at.
pub fn parse_rfc3339(text: &str) -> Result<SystemTime, ParseError> {
    let bytes = text.as_bytes();
    check_min_len(bytes)?;
    let (year, month, day) = parse_date(bytes)?;
    let (hour, minute, second) = parse_time(bytes)?;
    let mut cursor = 19;
    let nanos = parse_fraction(bytes, &mut cursor)?;
    let offset_minutes = parse_offset(bytes, cursor)?;

    // Every field is bounded before it gets here: a four-digit year is at most
    // `2_932_896` days from the epoch, each clock field is two digits, and the
    // offset is at most `99 * 60 + 99` minutes. The largest intermediate is
    // therefore about `2.53e11`, sixteen orders of magnitude below `i64::MAX`,
    // so no `saturating_*` below can saturate.
    let secs = days_from_civil(year, month, day)
        .saturating_mul(86_400)
        .saturating_add(i64::from(hour).saturating_mul(3_600))
        .saturating_add(i64::from(minute).saturating_mul(60))
        .saturating_add(i64::from(second))
        .saturating_sub(offset_minutes.saturating_mul(60));
    Ok(from_unix_parts(secs, nanos))
}
