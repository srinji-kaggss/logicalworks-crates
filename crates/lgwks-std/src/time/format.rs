//! RFC 3339 formatting helpers.
//!
//! Bridges `std::time::SystemTime` and the `(seconds, nanoseconds)` pair the
//! civil-calendar routines in [`super::calendar`] work with, and renders that
//! pair as RFC 3339 UTC text.

use super::calendar::civil_from_days;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Nanoseconds in one second.
///
/// `Duration::subsec_nanos` returns a value in `0..NANOS_PER_SECOND`, and this
/// is the carry point when a caller hands [`from_unix_parts`] a nanosecond
/// count that is not already normalised.
const NANOS_PER_SECOND: u32 = 1_000_000_000;

/// Seconds in one day: the divisor that turns an epoch-second count into a
/// civil day number and back.
const SECONDS_PER_DAY: i64 = 86_400;

/// Truncating integer division that cannot trap.
///
/// `clippy::integer_division` forbids the bare `/` operator, and every call
/// site divides by a non-zero constant, so the `None` arm of `checked_div` is
/// unreachable and exists only to keep the function total.
fn divide(numerator: u32, denominator: u32) -> u32 {
    numerator.checked_div(denominator).unwrap_or(0)
}

/// Appends one decimal digit: the low digit of `value`.
///
/// `char::from_digit` rejects anything outside `0..10`, so reducing with `% 10`
/// first makes the `None` arm unreachable rather than a panic.
fn push_digit(out: &mut String, value: u32) {
    out.push(char::from_digit(value % 10, 10).unwrap_or('0'));
}

/// The ASCII byte for the low decimal digit of `value`.
///
/// `value % 10` is `0..=9`, so the result is always in `b'0'..=b'9'`; the
/// saturating add states that bound rather than relying on it.
fn digit_byte(value: u32) -> u8 {
    b'0'.saturating_add(u8::try_from(value % 10).unwrap_or(0))
}

/// Splits an instant into whole seconds since the epoch and nanoseconds.
///
/// Nanoseconds are always in `0..1_000_000_000` and never negative: a
/// pre-epoch instant is normalised so the second count carries the sign and the
/// nanosecond remainder points *forward* in time. That normalisation is what
/// lets [`from_unix_parts`] invert this exactly.
///
/// # Saturation
///
/// `Duration::as_secs` is a `u64`, so a `SystemTime` more than `i64::MAX`
/// seconds from the epoch (about 292 billion years) has no `i64` second count
/// and is reported as `i64::MAX`. No operating-system clock and no RFC 3339
/// stamp reaches that point.
#[must_use]
pub fn unix_parts(at: SystemTime) -> (i64, u32) {
    match at.duration_since(UNIX_EPOCH) {
        Ok(duration_since_epoch) => (
            i64::try_from(duration_since_epoch.as_secs()).unwrap_or(i64::MAX),
            duration_since_epoch.subsec_nanos(),
        ),
        Err(before) => split_pre_epoch(before.duration()),
    }
}

/// Splits a duration measured *backwards* from the epoch into a signed second
/// count and a forward nanosecond remainder.
///
/// `UNIX_EPOCH - 1.5s` is the instant `-2s + 0.5s`: the second count rounds
/// down to `-2` and the remainder is `500_000_000`. Keeping the remainder
/// non-negative is what makes the pair round-trip through
/// [`from_unix_parts`], and what makes `to_rfc3339` render a pre-epoch instant
/// with the same fractional digits it was parsed from.
fn split_pre_epoch(duration: Duration) -> (i64, u32) {
    // `Duration::as_secs` is a `u64`; see `unix_parts` for the saturation note.
    let whole_seconds = i64::try_from(duration.as_secs()).unwrap_or(i64::MAX);
    let nanos = duration.subsec_nanos();
    if nanos == 0 {
        // Whole-second instant: `UNIX_EPOCH - whole_seconds` has no fraction to
        // normalise, so no second needs to be borrowed.
        (whole_seconds.saturating_neg(), 0)
    } else {
        // `nanos` is `1..1_000_000_000`, so exactly one second is borrowed and
        // the forward remainder is `1_000_000_000 - nanos`, in
        // `1..1_000_000_000`.
        (
            whole_seconds.saturating_neg().saturating_sub(1),
            NANOS_PER_SECOND.saturating_sub(nanos),
        )
    }
}

/// Converts seconds and nanoseconds into a `SystemTime`.
///
/// The inverse of [`unix_parts`] for every pair that function can produce:
/// `nanos` is read as a forward remainder, and a negative `secs` borrows a
/// second when `nanos` is non-zero.
///
/// Returns `UNIX_EPOCH` when the instant falls outside the range the platform
/// clock can represent; see [`try_from_unix_parts`] for the fallible form and
/// for how narrow that range is.
#[must_use]
pub fn from_unix_parts(secs: i64, nanos: u32) -> SystemTime {
    try_from_unix_parts(secs, nanos).unwrap_or(UNIX_EPOCH)
}

/// Converts seconds and nanoseconds into a `SystemTime`, or [`None`] when the
/// instant is outside the range the platform clock can represent.
///
/// This is the fallible form of [`from_unix_parts`]; the normalisation rules
/// are documented there.
///
/// A `nanos` value of `1_000_000_000` or more is folded into whole seconds
/// rather than passed to `Duration::new`, which would panic on it.
#[must_use]
pub fn try_from_unix_parts(secs: i64, nanos: u32) -> Option<SystemTime> {
    // Fold any nanosecond overflow into whole seconds up front so `Duration::new`
    // — which panics when `nanos >= 1_000_000_000` — never sees an out-of-range
    // count. The divisor is a non-zero constant, so neither `checked_*` fails.
    let carry = nanos.checked_div(NANOS_PER_SECOND).unwrap_or(0);
    let fraction = nanos.checked_rem(NANOS_PER_SECOND).unwrap_or(0);
    // `carry` is `0..=4`, so this can only fail for `secs` already at `i64::MAX`.
    let secs = secs.checked_add(i64::from(carry))?;
    if secs >= 0 {
        // Guarded non-negative, so the widening to `u64` is exact.
        let whole = u64::try_from(secs).unwrap_or(0);
        UNIX_EPOCH.checked_add(Duration::new(whole, fraction))
    } else if fraction == 0 {
        UNIX_EPOCH.checked_sub(Duration::new(secs.unsigned_abs(), 0))
    } else {
        // `secs < 0` here, so `unsigned_abs() >= 1` and the borrow cannot
        // underflow; `fraction` is `1..1_000_000_000`, so the forward remainder
        // is exact.
        UNIX_EPOCH.checked_sub(Duration::new(
            secs.unsigned_abs().saturating_sub(1),
            NANOS_PER_SECOND.saturating_sub(fraction),
        ))
    }
}

/// Appends the low two decimal digits of `value`, most significant first.
///
/// Only `value / 10` and `value % 10` are read, so any `u32` renders as its
/// last two digits without overflow. Callers that need the *high* digits of a
/// wider value (such as [`write_four_digits`]) shift it down first.
fn write_two_digits(out: &mut String, value: u32) {
    push_digit(out, divide(value, 10));
    push_digit(out, value % 10);
}

/// Appends the low four decimal digits of `value`, most significant first.
///
/// `value` is `0..=9999` at every call site (a four-digit RFC 3339 year), so
/// the two halves are `value / 100` and `value % 100`, each in `0..=99`.
fn write_four_digits(out: &mut String, value: u32) {
    write_two_digits(out, divide(value, 100));
    write_two_digits(out, value % 100);
}

/// Appends the nine-digit zero-padded decimal form of `nanos` into `buf` and
/// returns the length with trailing zeros trimmed.
///
/// `nanos` is `0..1_000_000_000` (a `Duration` subsecond count), so the trimmed
/// length is in `0..=9`: `0` when every digit is zero, and `9` for a nanosecond
/// count that is not a multiple of ten. Trimming is what turns `500_000_000`
/// into `.5` and `120_000_000` into `.12`, matching what the parser accepts.
fn extract_fraction_digits(nanos: u32, buf: &mut [u8; 9]) -> usize {
    let mut remainder = nanos;
    for index in (0..9).rev() {
        buf[index] = digit_byte(remainder % 10);
        remainder = divide(remainder, 10);
    }
    let mut length: usize = 9;
    // `length > 0` on entry to each iteration, so `length - 1` is in bounds and
    // cannot underflow.
    while length > 0 && buf[length.saturating_sub(1)] == b'0' {
        length = length.saturating_sub(1);
    }
    length
}

/// Appends the trimmed fractional digits of `nanos`.
///
/// The leading `.` belongs to the caller; nothing is appended when every digit
/// is zero, because an all-zero fraction carries no information.
fn write_fraction(out: &mut String, nanos: u32) {
    let mut buf = [0u8; 9];
    let length = extract_fraction_digits(nanos, &mut buf);
    for &byte in &buf[..length] {
        // `digit_byte` only ever produces `b'0'..=b'9'`, so the widening to
        // `char` is exact.
        out.push(char::from(byte));
    }
}

/// Appends `YYYY-MM-DD` for a proleptic Gregorian date.
///
/// A year in `0..=9999` takes the fixed four-digit path. Any other year — only
/// reachable from [`to_rfc3339`] for instants billions of years from the epoch
/// — falls back to `format_args!("{year:04}")`, which prints the sign and pads
/// the magnitude, so a negative year renders as `-001`.
fn write_civil(out: &mut String, year: i64, month: u32, day: u32) {
    if (0..=9999).contains(&year) {
        // Guarded non-negative and below 10_000, so the narrowing is exact.
        write_four_digits(out, u32::try_from(year).unwrap_or(0));
    } else {
        use std::fmt::Write;
        // `String`'s `fmt::Write` implementation formats into an in-memory
        // buffer and cannot fail, so there is no error to report. Binding the
        // `Result` consumes it without an `expect`, which would be a panic path
        // in library code.
        let _written = out.write_fmt(format_args!("{year:04}"));
    }
    out.push('-');
    write_two_digits(out, month);
    out.push('-');
    write_two_digits(out, day);
}

/// Appends `THH:MM:SS`.
///
/// The `T` separator and the fixed two-digit widths are what RFC 3339 requires
/// for a UTC stamp; a lower-case `t` is accepted on input but never emitted.
fn write_time_parts(out: &mut String, hour: u32, min: u32, sec: u32) {
    out.push('T');
    write_two_digits(out, hour);
    out.push(':');
    write_two_digits(out, min);
    out.push(':');
    write_two_digits(out, sec);
}

/// Appends the optional `.` fraction and the mandatory `Z` UTC designator.
///
/// The fraction is omitted entirely when `nanos` is zero, so a whole-second
/// instant renders as `...T00:00:00Z` rather than with nine trailing zeros.
fn write_optional_fraction(out: &mut String, nanos: u32) {
    if nanos > 0 {
        out.push('.');
        write_fraction(out, nanos);
    }
    out.push('Z');
}

/// Formats a `SystemTime` as an RFC 3339 UTC string with fractional seconds.
///
/// The result is always UTC (`Z`) and always round-trips through
/// [`super::parse::parse_rfc3339`]: the fraction is omitted when zero and
/// trailing zeros are trimmed otherwise, which is exactly the set of forms the
/// parser accepts.
#[must_use]
pub fn to_rfc3339(at: SystemTime) -> String {
    let (secs, nanos) = unix_parts(at);
    // Floor division maps an epoch-second count onto the civil day that
    // contains it, for negative (pre-epoch) seconds too. The divisor is a
    // non-zero constant, so `div_euclid` cannot trap.
    let (year, month, day) = civil_from_days(secs.div_euclid(SECONDS_PER_DAY));
    // The remainder is in `0..SECONDS_PER_DAY`, so the narrowing to `u32` is
    // exact.
    let rem = u32::try_from(secs.rem_euclid(SECONDS_PER_DAY)).unwrap_or(0);
    let hour = divide(rem, 3_600);
    let min = divide(rem % 3_600, 60);
    let sec = rem % 60;

    // "1970-01-01T00:00:00.123456789Z" is 30 bytes; 35 leaves room for a
    // five-digit year and the fraction without a reallocation.
    let mut out = String::with_capacity(35);
    write_civil(&mut out, year, month, day);
    write_time_parts(&mut out, hour, min, sec);
    write_optional_fraction(&mut out, nanos);
    out
}
