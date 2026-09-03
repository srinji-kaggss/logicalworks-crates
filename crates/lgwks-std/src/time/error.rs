//! RFC 3339 parsing and calendar field error types.

use std::error::Error;
use std::fmt;

/// Which calendar or clock field failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

fn field_name(field: &Field) -> &'static str {
    match field {
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

fn fmt_too_short(f: &mut fmt::Formatter<'_>, len: usize) -> fmt::Result {
    write!(f, "RFC 3339 input too short: {len} bytes")
}

fn fmt_malformed(f: &mut fmt::Formatter<'_>, at: usize, byte: u8) -> fmt::Result {
    write!(f, "unexpected character {byte:?} at offset {at}")
}

fn fmt_non_digit(f: &mut fmt::Formatter<'_>, field: Field, at: usize, byte: u8) -> fmt::Result {
    write!(f, "non-digit character {byte:?} in {field} at offset {at}")
}

fn fmt_out_of_range(
    f: &mut fmt::Formatter<'_>,
    field: Field,
    value: u32,
    min: u32,
    max: u32,
) -> fmt::Result {
    write!(f, "{field} {value} out of range {min}..={max}")
}

fn fmt_fraction_width(f: &mut fmt::Formatter<'_>, digits: usize) -> fmt::Result {
    write!(f, "fractional seconds width {digits} out of range 1..=9")
}

fn fmt_missing_offset(f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "missing timezone offset ('Z' or '+-HH:MM')")
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { len, at: _ } => fmt_too_short(f, *len),
            Self::Malformed { at, byte } => fmt_malformed(f, *at, *byte),
            Self::NonDigit { field, at, byte } => fmt_non_digit(f, *field, *at, *byte),
            Self::OutOfRange {
                field,
                value,
                min,
                max,
                at: _,
            } => fmt_out_of_range(f, *field, *value, *min, *max),
            Self::FractionWidth { digits, at: _ } => fmt_fraction_width(f, *digits),
            Self::MissingOffset { at: _ } => fmt_missing_offset(f),
        }
    }
}

impl Error for ParseError {}
