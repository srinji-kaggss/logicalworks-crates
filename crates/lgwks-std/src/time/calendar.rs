//! Proleptic Gregorian calendar date and leap year calculations.
//!
//! Provides conversion between civil dates (`year`, `month`, `day`) and total days
//! elapsed since the Unix epoch (`1970-01-01`), accounting for 400-year leap cycles.
//!
//! # Arithmetic domain
//!
//! The conversions are Howard Hinnant's `days_from_civil` / `civil_from_days`
//! pair, which is exact over the entire proleptic Gregorian calendar. Every
//! step below is written with `checked_*`, `saturating_*` or `div_euclid`
//! rather than the bare operators, because this workspace forbids
//! `clippy::arithmetic_side_effects`; the comment at each site names the bound
//! that makes the choice exact rather than merely non-panicking.
//!
//! The parser accepts four-digit RFC 3339 years. These public conversions also
//! cover the wider `i64` day/year domains: [`civil_from_days`] is exact for
//! every `i64` day count, and [`days_from_civil`] computes in `i128` before
//! saturating only when its final day count is outside `i64`.

/// Days in one 400-year Gregorian era: `365 * 400 + 97` leap days.
const DAYS_PER_ERA: i64 = 146_097;

/// Days from `1970-01-01` back to the start of era 0 (`0000-03-01`), the
/// epoch the era decomposition below is written against.
const EPOCH_ERA_OFFSET: i64 = 719_468;

/// Whole 400-year eras inside [`EPOCH_ERA_OFFSET`]: `4 * 146_097 = 584_388`.
const EPOCH_WHOLE_ERAS: i64 = 4;

/// Days left over after [`EPOCH_WHOLE_ERAS`]: `719_468 - 584_388 = 135_080`.
const EPOCH_ERA_REMAINDER: i64 = 135_080;

/// Determines whether the given astronomical year index is a leap year (366 days).
///
/// `year` is an astronomical year index: year `0` is 1 BCE, and negative values
/// run backwards from there. The rule is the Gregorian one: every fourth year
/// is a leap year except century years, which are leap years only when
/// divisible by 400. It holds for negative years too, because `%` here is
/// only ever compared against zero, where truncation and flooring agree.
#[must_use]
pub fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Returns the number of days in the specified month (1..=12) for a given year.
///
/// Returns [`None`] when `month` is outside `1..=12`, so an out-of-range month
/// is a value the caller must handle rather than a panic or a zero inside the
/// library.
#[must_use]
pub fn try_days_in_month(year: i64, month: u32) -> Option<u32> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if is_leap(year) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
///
/// `month` must be in `1..=12` and `day` a valid day of that month; the RFC
/// 3339 parser enforces both before calling. Under those preconditions the
/// complete mathematical count is computed before the result is narrowed to
/// `i64`; only a final value outside that range is saturated.
///
/// Invalid month/day values are outside the contract; this function does not
/// validate them. Its `i128` intermediates are wide enough for every `i64`
/// year and `u32` month/day.
#[must_use]
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    // Valid inputs have month in 1..=12 and day within that month. Even for
    // the full i64 year domain, each mathematical intermediate fits i128.
    let year = i128::from(year);
    let month = i128::from(month);
    let day = i128::from(day);
    let adjusted_year = if month <= 2 {
        year.saturating_sub(1)
    } else {
        year
    };
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year.saturating_sub(era.saturating_mul(400));
    let month_prime = if month > 2 {
        month.saturating_sub(3)
    } else {
        month.saturating_add(9)
    };
    // Every quotient below divides a non-negative numerator by a non-zero
    // constant, where `div_euclid` and truncating division agree; the method
    // form is what keeps the step total without a refusal arm.
    let day_of_year = month_prime
        .saturating_mul(153)
        .saturating_add(2)
        .div_euclid(5)
        .saturating_add(day)
        .saturating_sub(1);
    let day_of_era = year_of_era
        .saturating_mul(365)
        .saturating_add(year_of_era.div_euclid(4))
        .saturating_sub(year_of_era.div_euclid(100))
        .saturating_add(day_of_year);
    let days = era
        .saturating_mul(i128::from(DAYS_PER_ERA))
        .saturating_add(day_of_era)
        .saturating_sub(i128::from(EPOCH_ERA_OFFSET));
    // The complete count is computed in full before this narrowing, so the
    // only refusal left is a day count outside `i64`, and each side saturates
    // at the bound it crossed rather than reporting a nearby date.
    match i64::try_from(days) {
        Ok(narrowed) => narrowed,
        Err(_) => {
            let bound = if days.is_negative() {
                i64::MIN
            } else {
                i64::MAX
            };
            #[cfg(feature = "trace")]
            crate::trace::debug!(days = ?days, bound = bound, "days_from_civil: a day count outside i64 saturates at the bound it crossed");
            bound
        }
    }
}

/// Splits days-since-1970-01-01 into a 400-year era index and a `0..146_097`
/// day offset within that era.
///
/// This is the remainder-and-quotient of `days + 719_468` by `146_097`, but it
/// never forms that sum: `days + 719_468` overflows `i64` for the top ~1_970
/// years of the `i64` range, whereas the re-based form below stays inside it
/// for every input.
///
/// `719_468 = 4 * 146_097 + 135_080`, so the epoch sits four whole eras plus
/// `135_080` days into the cycle. Re-basing onto that boundary can carry at
/// most once, because `146_096 + 135_080 < 2 * 146_097`.
fn shifted_to_era(days_since_epoch: i64) -> (i64, i64) {
    // `div_euclid`/`rem_euclid` are a matched pair for negative inputs as well,
    // giving `days = era * 146_097 + within` with `within` in `0..146_097`. The
    // divisor is a non-zero constant, so neither can trap.
    let base_era = days_since_epoch.div_euclid(DAYS_PER_ERA);
    let within_era = days_since_epoch.rem_euclid(DAYS_PER_ERA);
    let rebased = within_era.saturating_add(EPOCH_ERA_REMAINDER);
    // `|base_era| <= i64::MAX / 146_097`, so neither `saturating_add` can
    // saturate.
    if rebased < DAYS_PER_ERA {
        (base_era.saturating_add(EPOCH_WHOLE_ERAS), rebased)
    } else {
        (
            base_era.saturating_add(EPOCH_WHOLE_ERAS).saturating_add(1),
            rebased.saturating_sub(DAYS_PER_ERA),
        )
    }
}

/// Converts an era index and a `0..146_097` day offset within it into a
/// March-based year and the `0..=365` day of that year.
///
/// `day_of_era` is bounded by `146_096`, which is what keeps every intermediate
/// here small: the year-of-era correction is `0..=399`, `era * 400` is a civil
/// year, and `365 * year_of_era` is at most `145_635`. Every `div_euclid` below
/// divides a non-negative numerator by a non-zero constant, where flooring and
/// truncating division agree.
fn era_to_year(day_of_era: i64, era: i64) -> (i64, i64) {
    let year_of_era = day_of_era
        .saturating_sub(day_of_era.div_euclid(1_460))
        .saturating_add(day_of_era.div_euclid(36_524))
        .saturating_sub(day_of_era.div_euclid(146_096))
        .div_euclid(365);
    let computed_year = year_of_era.saturating_add(era.saturating_mul(400));
    let day_of_year = day_of_era.saturating_sub(
        year_of_era
            .saturating_mul(365)
            .saturating_add(year_of_era.div_euclid(4))
            .saturating_sub(year_of_era.div_euclid(100)),
    );
    (computed_year, day_of_year)
}

/// Converts a March-based year and its `0..=365` day into a civil
/// `(year, month, day)`.
///
/// `day_of_year` is `0..=365`, so `month_prime` is `0..=12`, the day of month
/// is `1..=31`, and the month is `1..=12`. The month and day are returned as
/// the `u32` the public boundary names, computed in that domain from the
/// start: every `div_euclid` divides a non-negative numerator by a non-zero
/// constant, so nothing here needs a conversion or a refusal arm.
fn day_of_year_to_month_day(day_of_year: u32, computed_year: i64) -> (i64, u32, u32) {
    let month_prime = day_of_year
        .saturating_mul(5)
        .saturating_add(2)
        .div_euclid(153);
    let computed_day = day_of_year
        .saturating_sub(
            month_prime
                .saturating_mul(153)
                .saturating_add(2)
                .div_euclid(5),
        )
        .saturating_add(1);
    // Month 0 of the re-based year is March, so the last ten months wrap back
    // to the civil calendar.
    let computed_month = if month_prime < 10 {
        month_prime.saturating_add(3)
    } else {
        month_prime.saturating_sub(9)
    };
    let final_year = if computed_month <= 2 {
        computed_year.saturating_add(1)
    } else {
        computed_year
    };
    (final_year, computed_month, computed_day)
}

/// The era day offset `0..=146_096` as the `u32` the year pipeline divides in.
///
/// This is the one narrowing in the inverse conversion. `shifted_to_era` bounds
/// the offset to `0..=146_096`, so the value is inside `u32` and its four low
/// bytes are the value itself: reading them is total, where a checked
/// conversion would carry a refusal arm for a bound the caller has already
/// established, and where an `as` cast would make the bound invisible.
fn era_day_offset(day_of_era: i64) -> u32 {
    let bytes = day_of_era.to_le_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// The proleptic Gregorian date for a count of days since 1970-01-01.
///
/// The inverse of [`days_from_civil`]. Exact for every `i64` day count: the
/// re-basing in `shifted_to_era` avoids the one addition that could overflow,
/// and `146_097` is what bounds every remaining intermediate, so the reported
/// year stays inside `i64` even when the day count is at its extreme.
///
/// `shifted_to_era` is named rather than linked because it is private: a public
/// item's docs cannot reach a private one without `--document-private-items`,
/// and `rustdoc::private_intra_doc_links` is denied here.
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let (era, day_of_era) = shifted_to_era(days);
    let (computed_year, day_of_year) = era_to_year(day_of_era, era);
    day_of_year_to_month_day(era_day_offset(day_of_year), computed_year)
}
