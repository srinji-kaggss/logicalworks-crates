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

/// Truncating integer division that cannot trap.
///
/// `clippy::integer_division` forbids the bare `/` operator, and every call
/// site divides by a non-zero constant, so the `None` arm of `checked_div` is
/// unreachable and exists only to keep the function total.
fn divide(numerator: i64, denominator: i64) -> i64 {
    numerator.checked_div(denominator).unwrap_or(0)
}

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
/// is a value the caller must handle rather than a panic inside the library.
/// Use this in preference to [`days_in_month`] whenever the month has not
/// already been range-checked.
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

/// Returns the number of days in the specified month (1..=12) for a given year,
/// or `0` when `month` is outside `1..=12`.
///
/// `0` is a sentinel, not a month length: a caller that uses the result as an
/// inclusive upper bound for a day of month fails closed, because no day can
/// satisfy `1..=0`. Callers that need to distinguish "empty month" from
/// "invalid month" should use [`try_days_in_month`] instead.
#[must_use]
pub fn days_in_month(year: i64, month: u32) -> u32 {
    try_days_in_month(year, month).unwrap_or(0)
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
    let day_of_year = month_prime
        .saturating_mul(153)
        .saturating_add(2)
        .checked_div(5)
        .unwrap_or(0)
        .saturating_add(day)
        .saturating_sub(1);
    let day_of_era = year_of_era
        .saturating_mul(365)
        .saturating_add(year_of_era.checked_div(4).unwrap_or(0))
        .saturating_sub(year_of_era.checked_div(100).unwrap_or(0))
        .saturating_add(day_of_year);
    let days = era
        .saturating_mul(i128::from(DAYS_PER_ERA))
        .saturating_add(day_of_era)
        .saturating_sub(i128::from(EPOCH_ERA_OFFSET));
    i64::try_from(days).unwrap_or(if days.is_negative() {
        i64::MIN
    } else {
        i64::MAX
    })
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
/// year, and `365 * year_of_era` is at most `145_635`. Every division is
/// truncating on a non-negative numerator, matching `/` for the operands the
/// algorithm can supply.
fn era_to_year(day_of_era: i64, era: i64) -> (i64, i64) {
    let year_of_era = divide(
        day_of_era
            .saturating_sub(divide(day_of_era, 1_460))
            .saturating_add(divide(day_of_era, 36_524))
            .saturating_sub(divide(day_of_era, 146_096)),
        365,
    );
    let computed_year = year_of_era.saturating_add(era.saturating_mul(400));
    let day_of_year = day_of_era.saturating_sub(
        year_of_era
            .saturating_mul(365)
            .saturating_add(divide(year_of_era, 4))
            .saturating_sub(divide(year_of_era, 100)),
    );
    (computed_year, day_of_year)
}

/// Converts a March-based year and its `0..=365` day into a civil
/// `(year, month, day)`.
///
/// Returns `i64` components rather than the public `u32` ones so that the
/// narrowing happens once, at the public boundary, where the range is known.
/// `day_of_year` is `0..=365`, so `month_prime` is `0..=12`, the day of month
/// is `1..=31`, and the month is `1..=12`.
fn day_of_year_to_month_day(day_of_year: i64, computed_year: i64) -> (i64, i64, i64) {
    let month_prime = divide(day_of_year.saturating_mul(5).saturating_add(2), 153);
    let computed_day = day_of_year
        .saturating_sub(divide(month_prime.saturating_mul(153).saturating_add(2), 5))
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
    let (year, month, day) = day_of_year_to_month_day(day_of_year, computed_year);
    // `month` is `1..=12` and `day` is `1..=31` by construction, so both
    // narrowings are exact; the fallbacks are unreachable and exist only to
    // keep the conversion explicit instead of an `as` cast.
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}
