//! RFC 3339 formatting helpers.

use super::calendar::civil_from_days;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Splits an instant into whole seconds since the epoch and nanoseconds.
pub fn unix_parts(at: SystemTime) -> (i64, u32) {
    match at.duration_since(UNIX_EPOCH) {
        Ok(duration_since_epoch) => (
            duration_since_epoch.as_secs() as i64,
            duration_since_epoch.subsec_nanos(),
        ),
        Err(before) => split_pre_epoch(before.duration()),
    }
}

fn split_pre_epoch(duration: Duration) -> (i64, u32) {
    if duration.subsec_nanos() == 0 {
        (-(duration.as_secs() as i64), 0)
    } else {
        (
            (-(duration.as_secs() as i64) - 1),
            1_000_000_000 - duration.subsec_nanos(),
        )
    }
}

/// Converts seconds and nanoseconds into a `SystemTime`.
pub fn from_unix_parts(secs: i64, nanos: u32) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + Duration::new(secs as u64, nanos)
    } else if nanos == 0 {
        UNIX_EPOCH - Duration::new(secs.unsigned_abs(), 0)
    } else {
        UNIX_EPOCH - Duration::new(secs.unsigned_abs() - 1, 1_000_000_000 - nanos)
    }
}

fn write_two_digits(out: &mut String, val: u32) {
    out.push((b'0' + (val / 10) as u8) as char);
    out.push((b'0' + (val % 10) as u8) as char);
}

fn write_four_digits(out: &mut String, val: u32) {
    write_two_digits(out, val / 100);
    write_two_digits(out, val % 100);
}

fn extract_fraction_digits(nanos: u32, buf: &mut [u8; 9]) -> usize {
    let mut remainder = nanos;
    for i in (0..9).rev() {
        buf[i] = b'0' + (remainder % 10) as u8;
        remainder /= 10;
    }
    let mut len = 9;
    while len > 0 && buf[len - 1] == b'0' {
        len -= 1;
    }
    len
}

fn write_fraction(out: &mut String, nanos: u32) {
    let mut buf = [0u8; 9];
    let len = extract_fraction_digits(nanos, &mut buf);
    for &byte in &buf[..len] {
        out.push(byte as char);
    }
}

fn write_civil(out: &mut String, year: i64, month: u32, day: u32) {
    if (0..=9999).contains(&year) {
        write_four_digits(out, year as u32);
    } else {
        use std::fmt::Write;
        out.write_fmt(format_args!("{year:04}"))
            .expect("infallible String write");
    }
    out.push('-');
    write_two_digits(out, month);
    out.push('-');
    write_two_digits(out, day);
}

fn write_time_parts(out: &mut String, hour: u32, min: u32, sec: u32) {
    out.push('T');
    write_two_digits(out, hour);
    out.push(':');
    write_two_digits(out, min);
    out.push(':');
    write_two_digits(out, sec);
}

fn write_optional_fraction(out: &mut String, nanos: u32) {
    if nanos > 0 {
        out.push('.');
        write_fraction(out, nanos);
    }
    out.push('Z');
}

/// Formats a `SystemTime` as an RFC 3339 UTC string with fractional seconds.
pub fn to_rfc3339(at: SystemTime) -> String {
    let (secs, nanos) = unix_parts(at);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400) as u32;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;

    let mut out = String::with_capacity(35);
    write_civil(&mut out, year, month, day);
    write_time_parts(&mut out, hour, min, sec);
    write_optional_fraction(&mut out, nanos);
    out
}
