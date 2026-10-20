//! The log-log fit the `wide-*x10` rows are summarised by (#640).
//!
//! A ratio against the hand-rolled loop says the bot is slower; it does not say
//! whether the gap grows with the source count. The slope of `ln(ns/tick)`
//! against `ln(sources)` does: 1 is a cost linear in the sources, 2 is a cost
//! quadratic in them, and anything above the linear line is a term that was not
//! paid once per source.
//!
//! The fit is least squares over the measured medians, in its own module so the
//! arithmetic that a published sentence quotes is the arithmetic the rig ran,
//! with a unit test pinning both exponents.

/// The least-squares slope of `ln(y)` against `ln(x)`, and the intercept.
///
/// A slope of 1 is linear in the source count; 2 is quadratic. `None` for fewer
/// than two points, for every `x` identical (no line to fit), or for a
/// non-positive value (no logarithm): a fit is reported only where it exists,
/// because a slope printed for one point would be a number nobody measured.
pub(super) fn loglog(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    if points.len() < 2 {
        return None;
    }
    let count = points.len() as f64;
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    for &(x, y) in points {
        if x <= 0.0 || y <= 0.0 {
            return None;
        }
        sum_x += x.ln();
        sum_y += y.ln();
    }
    let mean_x = sum_x / count;
    let mean_y = sum_y / count;
    let mut covariance = 0.0;
    let mut variance = 0.0;
    for &(x, y) in points {
        let dx = x.ln() - mean_x;
        covariance += dx * (y.ln() - mean_y);
        variance += dx * dx;
    }
    if variance <= f64::EPSILON {
        return None;
    }
    let slope = covariance / variance;
    Some((slope, mean_y - slope * mean_x))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four times the sources for four times the cost is exponent 1 to the last
    /// bit, so the slope a sentence quotes is arithmetic and not an impression.
    #[test]
    fn the_fit_recovers_a_linear_exponent() {
        let points = [(256.0, 54_248.3), (1_024.0, 216_993.2)];
        let fitted = loglog(&points);
        assert!(fitted.is_some(), "two points on a line is a fit");
        if let Some((slope, _)) = fitted {
            assert!(
                (slope - 1.0).abs() < 1e-9,
                "four times both axes is exponent 1, got {slope}"
            );
        }
    }

    /// A squared family reads as 2, and a single point has no fit to report.
    #[test]
    fn the_fit_recovers_a_quadratic_exponent_and_refuses_one_point() {
        let points = [(256.0, 16.0), (1_024.0, 256.0), (4_096.0, 4_096.0)];
        let fitted = loglog(&points);
        assert!(
            fitted.is_some(),
            "a squared family is three points on a line"
        );
        if let Some((slope, _)) = fitted {
            assert!(
                (slope - 2.0).abs() < 1e-9,
                "a squared family is exponent 2, got {slope}"
            );
        }
        assert_eq!(loglog(&[(256.0, 1.0)]), None, "one point is not a fit");
    }
}
