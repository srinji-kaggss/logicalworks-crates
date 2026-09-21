//! RFC 3339 parsing and calendar field error types.

use std::error::Error;
use std::fmt;

/// Which calendar or clock field failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Field {
    /// Calendar year.
    Year,
    /// Calendar month, `1..=12`.
    Month,
    /// Day of month.
    Day,
    /// Hour of day, `0..=23`.
    Hour,
    /// Minute of hour, `0..=59`.
    Minute,
    /// Second of minute, `0..=60`.
    Second,
    /// Offset hours.
    OffsetHour,
    /// Offset minutes.
    OffsetMinute,
}

/// The lower-case name of a field as it appears in an error message.
///
/// Names are hyphenated (`offset-hour`) so the text reads as a single token
/// inside a sentence without needing quoting.
fn field_name(field: &Field) -> &'static str {
    match *field {
        Field::Year => "year",
        Field::Month => "month",
        Field::Day => "day",
        Field::Hour => "hour",
        Field::Minute => "minute",
        Field::Second => "second",
        Field::OffsetHour => "offset-hour",
        Field::OffsetMinute => "offset-minute",
    }
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(field_name(self))
    }
}

/// Why an RFC 3339 string failed to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    /// Input has fewer bytes than the minimum valid RFC 3339 stamp.
    TooShort {
        /// The observed byte length.
        len: usize,
        /// Offset where input ended.
        at: usize,
    },
    /// A character appeared where a specific separator or digit was required.
    Malformed {
        /// Offset in the input where the byte was found.
        at: usize,
        /// The byte observed.
        byte: u8,
    },
    /// A calendar or clock field contained a non-digit character.
    NonDigit {
        /// The field being parsed.
        field: Field,
        /// Offset in the input.
        at: usize,
        /// The non-digit byte observed.
        byte: u8,
    },
    /// A calendar or clock field was outside its valid numerical range.
    OutOfRange {
        /// The field that was out of range.
        field: Field,
        /// The numeric value observed.
        value: u32,
        /// Inclusive lower bound.
        min: u32,
        /// Inclusive upper bound.
        max: u32,
        /// Offset where field was parsed.
        at: usize,
    },
    /// The fractional seconds part had no digits or more than nine digits.
    FractionWidth {
        /// The digit count observed.
        digits: usize,
        /// Offset of fractional dot.
        at: usize,
    },
    /// No timezone indicator (`Z` or `±HH:MM`) was present at the end.
    MissingOffset {
        /// Offset where offset was expected.
        at: usize,
    },
}

/// Renders [`ParseError::TooShort`]: the observed length and the offset at
/// which the input ran out.
fn fmt_too_short(formatter: &mut fmt::Formatter<'_>, len: usize, at: usize) -> fmt::Result {
    write!(
        formatter,
        "RFC 3339 input too short: {len} bytes (ended at offset {at})"
    )
}

/// Renders [`ParseError::Malformed`]: the offending byte and where it sat.
///
/// The byte is printed with `{:?}` so a control character or a byte that is
/// not valid UTF-8 on its own is still readable.
fn fmt_malformed(formatter: &mut fmt::Formatter<'_>, at: usize, byte: u8) -> fmt::Result {
    write!(formatter, "unexpected character {byte:?} at offset {at}")
}

/// Renders [`ParseError::NonDigit`]: the field that was being read, the byte
/// that was not a digit, and the offset of that byte.
fn fmt_non_digit(
    formatter: &mut fmt::Formatter<'_>,
    field: Field,
    at: usize,
    byte: u8,
) -> fmt::Result {
    write!(
        formatter,
        "non-digit character {byte:?} in {field} at offset {at}"
    )
}

/// Renders [`ParseError::OutOfRange`]: the field, its value, the inclusive
/// bounds it failed, and the offset the field started at.
fn fmt_out_of_range(
    formatter: &mut fmt::Formatter<'_>,
    field: Field,
    value: u32,
    min: u32,
    max: u32,
    at: usize,
) -> fmt::Result {
    write!(
        formatter,
        "{field} {value} out of range {min}..={max} at offset {at}"
    )
}

/// Renders [`ParseError::FractionWidth`]: how many fractional digits were
/// found, and the offset of the `.` that introduced them.
fn fmt_fraction_width(formatter: &mut fmt::Formatter<'_>, digits: usize, at: usize) -> fmt::Result {
    write!(
        formatter,
        "fractional seconds width {digits} out of range 1..=9 at offset {at}"
    )
}

/// Renders [`ParseError::MissingOffset`]: the offset at which the timezone
/// designator was expected.
fn fmt_missing_offset(formatter: &mut fmt::Formatter<'_>, at: usize) -> fmt::Result {
    write!(
        formatter,
        "missing timezone offset ('Z' or '+-HH:MM') at offset {at}"
    )
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Every field of every variant is `Copy`, so matching on `*self` binds
        // them by value and no arm needs to dereference.
        match *self {
            Self::TooShort { len, at } => fmt_too_short(formatter, len, at),
            Self::Malformed { at, byte } => fmt_malformed(formatter, at, byte),
            Self::NonDigit { field, at, byte } => fmt_non_digit(formatter, field, at, byte),
            Self::OutOfRange {
                field,
                value,
                min,
                max,
                at,
            } => fmt_out_of_range(formatter, field, value, min, max, at),
            Self::FractionWidth { digits, at } => fmt_fraction_width(formatter, digits, at),
            Self::MissingOffset { at } => fmt_missing_offset(formatter, at),
        }
    }
}

impl Error for ParseError {}
