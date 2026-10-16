//! The few statistics a run reports, computed without a numeric cast: counts
//! reach a float only through `f64::from` on 32-bit halves, so a count never
//! loses a bit on the way to a median or an interval.

use lgwks_std::json::Value;

/// The two-sided 95% normal quantile the Wilson interval uses.
const Z95: f64 = 1.959_963_984_540_054;

/// The size of one 32-bit half.
const HALF: u64 = 4_294_967_296;

/// `value` as a `u32`, saturating at the top.
fn narrow(value: u64) -> u32 {
    let Ok(found) = u32::try_from(value) else {
        return u32::MAX;
    };
    found
}

/// `value` as an `f64`, exact up to 2^53.
pub(super) fn exact(value: u64) -> f64 {
    f64::from(narrow(value.div_euclid(HALF))) * 4_294_967_296.0
        + f64::from(narrow(value.rem_euclid(HALF)))
}

/// A count as a `u64`, saturating at the top.
pub(super) fn wide(count: usize) -> u64 {
    let Ok(found) = u64::try_from(count) else {
        return u64::MAX;
    };
    found
}

/// `numerator` over `denominator`, or `None` when there is no denominator.
pub(super) fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    if denominator == 0 {
        None
    } else {
        Some(exact(numerator) / exact(denominator))
    }
}

/// The median of whole numbers: an integer when it is one, a half otherwise.
pub(super) fn median_whole(values: &[u64]) -> Option<Value> {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len().div_euclid(2);
    if sorted.len().rem_euclid(2) == 1 {
        return sorted.get(middle).map(|found| Value::from(*found));
    }
    let high = *sorted.get(middle)?;
    let low = *sorted.get(middle.checked_sub(1)?)?;
    let sum = low.saturating_add(high);
    if sum.rem_euclid(2) == 0 {
        Some(Value::from(sum.div_euclid(2)))
    } else {
        Some(Value::from(exact(sum) / 2.0))
    }
}

/// The median of real numbers.
pub(super) fn median_real(values: &[f64]) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len().div_euclid(2);
    if sorted.len().rem_euclid(2) == 1 {
        return sorted.get(middle).copied();
    }
    let high = *sorted.get(middle)?;
    let low = *sorted.get(middle.checked_sub(1)?)?;
    Some((low + high) / 2.0)
}

/// The mean of whole numbers.
pub(super) fn mean_whole(values: &[u64]) -> Option<f64> {
    let sum = values
        .iter()
        .fold(0_u64, |total, value| total.saturating_add(*value));
    ratio(sum, wide(values.len()))
}

/// The 95% Wilson score interval for `passed` of `total`, as fractions.
///
/// Wilson rather than the normal approximation because the cells are small and
/// sit at 0 or `total` often: the normal interval collapses to a point there and
/// would let a cell of 10/10 and one of 8/10 read as different.
pub(super) fn wilson(passed: u64, total: u64) -> (f64, f64) {
    if total == 0 {
        return (0.0, 1.0);
    }
    let size = exact(total);
    let share = exact(passed) / size;
    let z_squared = Z95 * Z95;
    let scale = 1.0 + z_squared / size;
    let centre = (share + z_squared / (2.0 * size)) / scale;
    let half =
        (Z95 / scale) * (share * (1.0 - share) / size + z_squared / (4.0 * size * size)).sqrt();
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

#[cfg(test)]
mod tests {
    use super::{exact, mean_whole, median_real, median_whole, ratio, wide, wilson};
    use lgwks_std::json::Value;

    /// Whether two floats agree to a part in a million.
    fn close(left: f64, right: f64) -> bool {
        (left - right).abs() < 1e-6
    }

    /// Statistics.median of Python's standard library: the middle element, or the
    /// mean of the two middle ones (docs.python.org, `statistics.median`).
    #[test]
    fn the_median_of_whole_numbers_is_the_middle_or_the_mean_of_the_middle_two() {
        assert_eq!(
            median_whole(&[3, 1, 2]),
            Some(Value::from(2_u64)),
            "an odd count takes the middle element"
        );
        assert_eq!(
            median_whole(&[4, 2]),
            Some(Value::from(3_u64)),
            "an even count of an even sum is a whole number"
        );
        assert_eq!(
            median_whole(&[1, 2]),
            Some(Value::from(1.5_f64)),
            "an even count of an odd sum is a half"
        );
        assert_eq!(median_whole(&[]), None, "no values have no median");
    }

    #[test]
    fn the_median_of_real_numbers_matches_the_whole_number_rule() {
        assert!(
            median_real(&[2.5, 0.5, 1.5]).is_some_and(|found| close(found, 1.5)),
            "an odd count takes the middle"
        );
        assert!(
            median_real(&[1.0, 2.0]).is_some_and(|found| close(found, 1.5)),
            "an even count averages the middle two"
        );
        assert_eq!(median_real(&[]), None, "no values have no median");
    }

    #[test]
    fn counts_reach_floats_without_loss() {
        assert!(
            close(exact(9_007_199_254_740_991), 9_007_199_254_740_991.0),
            "2^53 - 1 is exact"
        );
        assert_eq!(wide(7), 7, "a small count is itself");
        assert_eq!(ratio(1, 0), None, "no denominator, no ratio");
        assert!(
            ratio(3, 6).is_some_and(|found| close(found, 0.5)),
            "three of six is a half"
        );
        assert!(
            mean_whole(&[0, 0, 3]).is_some_and(|found| close(found, 1.0)),
            "the mean is the sum over the count"
        );
    }

    /// Wilson 1927; the 10/10 and 8/10 rows are the worked examples of the
    /// interval's behaviour at the edge, which the normal interval gets wrong.
    #[test]
    fn the_wilson_interval_keeps_a_ten_of_ten_cell_below_one_and_apart_from_zero() {
        let (low, high) = wilson(10, 10);
        assert!(close(high, 1.0), "the upper end of 10/10 is one");
        assert!(
            low > 0.72 && low < 0.73,
            "the lower end of 10/10 is near 0.722"
        );
        let (eight_low, eight_high) = wilson(8, 10);
        assert!(
            eight_low < low && eight_high < 0.95,
            "8/10 sits below 10/10 and overlaps it"
        );
        let (none_low, none_high) = wilson(0, 0);
        assert!(
            close(none_low, 0.0) && close(none_high, 1.0),
            "no trials say nothing"
        );
        let (zero_low, zero_high) = wilson(0, 10);
        assert!(
            close(zero_low, 0.0) && zero_high > 0.27 && zero_high < 0.28,
            "the upper end of 0/10 is near 0.278"
        );
    }
}
