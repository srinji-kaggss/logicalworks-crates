//! Proleptic Gregorian calendar date and leap year calculations.
//!
//! Provides conversion between civil dates (`year`, `month`, `day`) and total days
//! elapsed since the Unix epoch (`1970-01-01`), accounting for 400-year leap cycles.

/// Determines whether the given astronomical year index is a leap year (366 days).
pub fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Returns the number of days in the specified month (1..=12) for a given year.
///
/// # Panics
///
/// Panics if `month` is not in `1..=12`.
pub fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => panic!("month must be 1..=12, got {month}"),
    }
}

fn civil_to_era(year: i64, month: u32) -> (i64, i64) {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    (era, year_of_era)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let (era, year_of_era) = civil_to_era(year, month);
    let month_prime = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn shifted_to_era(shifted_days: i64) -> (i64, i64) {
    let era = if shifted_days >= 0 {
        shifted_days
    } else {
        shifted_days - 146_096
    } / 146_097;
    let day_of_era = shifted_days - era * 146_097;
    (era, day_of_era)
}

fn era_to_year(day_of_era: i64, era: i64) -> (i64, i64) {
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let computed_year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    (computed_year, day_of_year)
}

fn day_of_year_to_month_day(day_of_year: i64, computed_year: i64) -> (i64, u32, u32) {
    let month_prime = (5 * day_of_year + 2) / 153;
    let computed_day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let computed_month = (if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    }) as u32;
    let final_year = if computed_month <= 2 {
        computed_year + 1
    } else {
        computed_year
    };
    (final_year, computed_month, computed_day)
}

/// The proleptic Gregorian date for a count of days since 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted_days = days + 719_468;
    let (era, day_of_era) = shifted_to_era(shifted_days);
    let (computed_year, day_of_year) = era_to_year(day_of_era, era);
    day_of_year_to_month_day(day_of_year, computed_year)
}
