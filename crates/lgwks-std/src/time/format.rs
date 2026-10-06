//! RFC 3339 formatting helpers.
//!
//! Bridges `std::time::SystemTime` and the `(seconds, nanoseconds)` pair the
//! civil-calendar routines in [`super::calendar`] work with, and renders that
//! pair as RFC 3339 UTC text.

use super::calendar::civil_from_days;
use super::error::{FormatError, UnixTimeError};
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

/// The low byte of a value the caller has reduced to `0..=255`.
///
/// RFC 3339's rendering reduces every quantity to a decimal digit or a two-digit
/// pair before it reaches a byte, so the low byte is the value itself. Reading
/// it is total where a checked conversion would carry a refusal arm for a bound
/// the call sites have already established, and where an `as` cast would hide
/// the bound from the reader.
fn low_byte(value: i64) -> u8 {
    value.to_le_bytes()[0]
}

/// Appends one decimal digit: the low digit of `value`.
///
/// `value` is `0..=9` at every call site, so the digit is a `char` whose code
/// point is the ASCII `'0'` plus the digit's own value.
fn push_digit(out: &mut String, value: i64) {
    out.push(char::from(
        b'0'.saturating_add(low_byte(value.rem_euclid(10))),
    ));
}

/// The ASCII byte for the low decimal digit of `value`.
///
/// `value % 10` is `0..=9`, so the result is always in `b'0'..=b'9'`; the
/// saturating add states that bound rather than relying on it.
fn digit_byte(value: i64) -> u8 {
    b'0'.saturating_add(low_byte(value.rem_euclid(10)))
}

/// Splits an instant into whole seconds since the epoch and nanoseconds.
///
/// Nanoseconds are always in `0..1_000_000_000` and never negative: a
/// pre-epoch instant is normalised so the second count carries the sign and the
/// nanosecond remainder points *forward* in time. That normalisation is what
/// lets [`from_unix_parts`] invert this exactly.
///
/// Returns an error when the instant is outside the signed Unix-seconds range.
pub fn unix_parts(at: SystemTime) -> Result<(i64, u32), UnixTimeError> {
    match at.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            // The narrowing is checked rather than assumed: a whole-second
            // count past `i64::MAX` is the only way this can fail, and the
            // `TryFromIntError` it produces travels on inside the variant so
            // the count that did not fit is still reachable after the
            // translation.
            let seconds = i64::try_from(duration.as_secs()).map_err(|cause| {
                UnixTimeError::SystemTimeOutsideI64Range {
                    before_epoch: false,
                    cause,
                }
            })?;
            Ok((seconds, duration.subsec_nanos()))
        }
        Err(before) => {
            let duration = before.duration();
            if duration.as_secs() == i64::MAX.unsigned_abs().saturating_add(1)
                && duration.subsec_nanos() == 0
            {
                return Ok((i64::MIN, 0));
            }
            // One whole-second count does not fit `i64`, and the guard above
            // answers exactly it. The `Err` arm therefore does carry the
            // seconds count that overflowed, so it is attached to the refusal
            // rather than dropped on the way through.
            let whole = match i64::try_from(duration.as_secs()) {
                Ok(whole) => whole,
                Err(cause) => {
                    let refusal = Err(UnixTimeError::SystemTimeOutsideI64Range {
                        before_epoch: true,
                        cause,
                    });
                    #[cfg(feature = "trace")]
                    crate::trace::debug!(error = ?refusal.as_ref().err(), "unix_parts: returning an error to the caller");
                    return refusal;
                }
            };
            let nanos = duration.subsec_nanos();
            let seconds = if nanos == 0 {
                whole.checked_neg()
            } else {
                whole.checked_neg().and_then(|value| value.checked_sub(1))
            };
            let seconds = match seconds {
                Some(seconds) => seconds,
                None => {
                    // Only negation can refuse here, and it refuses for exactly
                    // one magnitude: `i64::MIN`. Naming it in its own variant is
                    // what keeps the fact observable — fabricating a conversion
                    // failure here would attach a cause that never happened.
                    let refusal = Err(UnixTimeError::NegationOverflow {
                        magnitude: whole.unsigned_abs(),
                    });
                    #[cfg(feature = "trace")]
                    crate::trace::debug!(error = ?refusal.as_ref().err(), "unix_parts: returning an error to the caller");
                    return refusal;
                }
            };
            Ok((
                seconds,
                if nanos == 0 {
                    0
                } else {
                    NANOS_PER_SECOND.saturating_sub(nanos)
                },
            ))
        }
    }
}

/// Lossy compatibility form of [`unix_parts`], which clamps an instant beyond
/// the signed Unix-seconds range. Prefer the checked function for decisions or
/// serialized output.
#[deprecated(note = "lossy; use unix_parts and handle UnixTimeError")]
#[must_use]
pub fn unix_parts_lossy(at: SystemTime) -> (i64, u32) {
    // The clamp *is* this function's contract, so it is written as the answer to
    // a refusal rather than hidden behind a default: a whole-second count too
    // large to negate saturates low, and every other refusal saturates high.
    match unix_parts(at) {
        Ok(parts) => parts,
        Err(error) => match error {
            UnixTimeError::SystemTimeOutsideI64Range {
                before_epoch: true, ..
            } => (i64::MIN, 0),
            UnixTimeError::SystemTimeOutsideI64Range {
                before_epoch: false,
                ..
            }
            | UnixTimeError::NegationOverflow { .. }
            | UnixTimeError::SecondsOverflow { .. }
            | UnixTimeError::SystemTimeOutOfRange { .. } => (i64::MAX, 0),
        },
    }
}

/// Converts seconds and nanoseconds into a [`SystemTime`], or returns why the
/// requested instant cannot be represented.
///
/// The inverse of [`unix_parts`] for every pair that function can produce:
/// `nanos` is read as a forward remainder, and a negative `secs` borrows a
/// second when `nanos` is non-zero.
///
/// Nanosecond values at or above one billion are normalized into seconds. If
/// that carry overflows or the platform clock rejects the resulting instant,
/// the error preserves the failed input or normalized pair.
///
/// ```rust
/// use lgwks_std::time::format::{from_unix_parts, to_rfc3339};
///
/// let instant = from_unix_parts(0, 0)?;
/// assert_eq!(to_rfc3339(instant)?, "1970-01-01T00:00:00Z");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn from_unix_parts(secs: i64, nanos: u32) -> Result<SystemTime, UnixTimeError> {
    // `nanos` is unsigned and the divisor is a non-zero constant, so the euclid
    // forms are total and carry the nanosecond overflow into whole seconds.
    let carry = i64::from(nanos.div_euclid(NANOS_PER_SECOND));
    let fraction = nanos.rem_euclid(NANOS_PER_SECOND);
    let normalized_secs = secs
        .checked_add(carry)
        .ok_or(UnixTimeError::SecondsOverflow {
            seconds: secs,
            nanoseconds: nanos,
        })?;
    let instant = if normalized_secs >= 0 {
        // A non-negative `i64` and its `u64` bit pattern are the same number, so
        // the widening is a reinterpretation of the same eight bytes rather than
        // a conversion with a refusal to answer.
        let whole = u64::from_le_bytes(normalized_secs.to_le_bytes());
        UNIX_EPOCH.checked_add(Duration::new(whole, fraction))
    } else if fraction == 0 {
        UNIX_EPOCH.checked_sub(Duration::new(normalized_secs.unsigned_abs(), 0))
    } else {
        UNIX_EPOCH.checked_sub(Duration::new(
            normalized_secs.unsigned_abs().saturating_sub(1),
            NANOS_PER_SECOND.saturating_sub(fraction),
        ))
    };
    instant.ok_or(UnixTimeError::SystemTimeOutOfRange {
        seconds: normalized_secs,
        nanoseconds: fraction,
    })
}

/// Lossy compatibility form of [`from_unix_parts`]. It returns the Unix epoch
/// when the input is outside the platform clock's range. Prefer the checked
/// function for correctness-sensitive code.
#[deprecated(note = "lossy; use from_unix_parts and handle UnixTimeError")]
#[must_use]
pub fn from_unix_parts_lossy(secs: i64, nanos: u32) -> SystemTime {
    // The clamp is this function's documented contract, so it is the answer to a
    // refusal rather than a default on one: a pair the platform clock cannot
    // name is reported as the epoch, and the loss is emitted so a caller that
    // reaches for this by mistake can see that it happened.
    match from_unix_parts(secs, nanos) {
        Ok(instant) => instant,
        Err(error) => {
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?error, "from_unix_parts_lossy: clamping a refused pair to the Unix epoch");
            #[cfg(not(feature = "trace"))]
            let _ = error;
            UNIX_EPOCH
        }
    }
}

/// Appends the low two decimal digits of `value`, most significant first.
///
/// `value` is `0..=99` at every call site, so the two digits are the euclid
/// quotient and remainder of ten with no overflow and no negative digit.
fn write_two_digits(out: &mut String, value: i64) {
    push_digit(out, value.div_euclid(10));
    push_digit(out, value.rem_euclid(10));
}

/// Appends the low four decimal digits of `value`, most significant first.
///
/// `value` is `0..=9999` at every call site (a four-digit RFC 3339 year), so
/// the two halves are `value / 100` and `value % 100`, each in `0..=99`.
fn write_four_digits(out: &mut String, value: i64) {
    write_two_digits(out, value.div_euclid(100));
    write_two_digits(out, value.rem_euclid(100));
}

/// Appends the nine-digit zero-padded decimal form of `nanos` into `buf` and
/// returns the length with trailing zeros trimmed.
///
/// `nanos` is `0..1_000_000_000` (a `Duration` subsecond count), so the trimmed
/// length is in `0..=9`: `0` when every digit is zero, and `9` for a nanosecond
/// count that is not a multiple of ten. Trimming is what turns `500_000_000`
/// into `.5` and `120_000_000` into `.12`, matching what the parser accepts.
fn extract_fraction_digits(nanos: i64, buf: &mut [u8; 9]) -> usize {
    let mut remainder = nanos;
    for index in (0..9).rev() {
        buf[index] = digit_byte(remainder.rem_euclid(10));
        remainder = remainder.div_euclid(10);
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
fn write_fraction(out: &mut String, nanos: i64) {
    let mut buf = [0u8; 9];
    let length = extract_fraction_digits(nanos, &mut buf);
    for &byte in &buf[..length] {
        // `digit_byte` only ever produces `b'0'..=b'9'`, so the widening to
        // `char` is exact.
        out.push(char::from(byte));
    }
}

/// Appends `YYYY-MM-DD` for an RFC 3339 year already checked in `0..=9999`.
fn write_civil(out: &mut String, year: i64, month: u32, day: u32) {
    write_four_digits(out, year);
    out.push('-');
    write_two_digits(out, i64::from(month));
    out.push('-');
    write_two_digits(out, i64::from(day));
}

/// Appends `THH:MM:SS`.
///
/// The `T` separator and the fixed two-digit widths are what RFC 3339 requires
/// for a UTC stamp; a lower-case `t` is accepted on input but never emitted.
fn write_time_parts(out: &mut String, hour: i64, min: i64, sec: i64) {
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
fn write_optional_fraction(out: &mut String, nanos: i64) {
    if nanos > 0 {
        out.push('.');
        write_fraction(out, nanos);
    }
    out.push('Z');
}

/// Formats a `SystemTime` as an RFC 3339 UTC string with fractional seconds.
///
/// The result is UTC (`Z`) and round-trips through
/// [`super::parse::parse_rfc3339`] when the UTC year is within `0000..=9999`.
/// Instants outside that range, or outside the signed Unix-seconds domain, are
/// refused instead of emitting extended-year text or a saturated timestamp.
pub fn to_rfc3339(at: SystemTime) -> Result<String, FormatError> {
    let (secs, nanos) = unix_parts(at).map_err(FormatError::UnixTime)?;
    // Floor division maps an epoch-second count onto the civil day that
    // contains it, for negative (pre-epoch) seconds too. The divisor is a
    // non-zero constant, so `div_euclid` cannot trap.
    let (year, month, day) = civil_from_days(secs.div_euclid(SECONDS_PER_DAY));
    if !(0..=9999).contains(&year) {
        let refusal = Err(FormatError::YearOutsideRfc3339 { year });
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "to_rfc3339: returning an error to the caller");
        return refusal;
    }
    // The remainder is in `0..SECONDS_PER_DAY` and every field below it is a
    // two-digit count, so the whole time-of-day is rendered in the domain the
    // day count is already in and nothing here needs a narrowing conversion.
    let rem = secs.rem_euclid(SECONDS_PER_DAY);
    let hour = rem.div_euclid(3_600);
    let min = rem.rem_euclid(3_600).div_euclid(60);
    let sec = rem.rem_euclid(60);

    // "1970-01-01T00:00:00.123456789Z" is 30 bytes; 35 leaves room for a
    // five-digit year and the fraction without a reallocation.
    let mut out = String::with_capacity(35);
    write_civil(&mut out, year, month, day);
    write_time_parts(&mut out, hour, min, sec);
    write_optional_fraction(&mut out, i64::from(nanos));
    Ok(out)
}
