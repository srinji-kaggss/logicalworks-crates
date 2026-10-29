//! The mark's twenty-four beams as published, the geometry derived from them,
//! and the small numeric and formatting helpers the effects share.

use crate::ink_lab::Shape;

/// Geometry of one of the mark's twenty-four beams.
#[derive(Clone, Copy, Debug)]
pub(super) struct Beam {
    /// The published four corners, emitted verbatim once the mark is crisp.
    pub(super) points: [[f64; 2]; 4],
    /// Narrow-end centre.
    pub(super) start: [f64; 2],
    /// Wide-end centre.
    pub(super) end: [f64; 2],
    /// Half-width at the narrow end.
    pub(super) half_start: f64,
    /// Half-width at the wide end.
    pub(super) half_end: f64,
    /// Unit normal x.
    pub(super) normal_x: f64,
    /// Unit normal y.
    pub(super) normal_y: f64,
}

impl Beam {
    /// Number of soft blobs that fill this beam while it is liquid.
    ///
    /// Returned as a float so the count can be compared against a float loop
    /// index without an `as` cast, which the crate forbids.
    pub(super) fn blob_count(&self) -> f64 {
        let length =
            ((self.end[0] - self.start[0]).powi(2) + (self.end[1] - self.start[1]).powi(2)).sqrt();
        let raw = (length / 2.6).round();
        if raw < 5.0 { 5.0 } else { raw }
    }

    /// The crisp beam: the brand's published polygon, verbatim.
    pub(super) const fn quad(&self) -> Shape {
        Shape::Quad {
            points: self.points,
        }
    }
}

/// The brand's twenty-four beam polygons, clockwise from twelve o'clock.
///
/// The published `points` attributes, as numbers: each beam is four corners,
/// narrow end first. Numeric at compile time so malformed geometry cannot be
/// parsed into an invented zero.
const BEAM_POINTS: [[[f64; 2]; 4]; 24] = [
    [[59.34, 51.0], [58.53, 40.0], [61.47, 40.0], [60.66, 51.0]],
    [
        [61.69, 51.14],
        [63.88, 39.67],
        [66.81, 40.45],
        [62.97, 51.48],
    ],
    [
        [63.93, 51.88],
        [69.42, 40.51],
        [72.17, 42.1],
        [65.07, 52.54],
    ],
    [[65.9, 53.17], [74.87, 42.78], [77.22, 45.13], [66.83, 54.1]],
    [
        [67.46, 54.93],
        [79.83, 46.52],
        [81.58, 49.57],
        [68.12, 56.07],
    ],
    [
        [68.52, 57.03],
        [83.89, 51.68],
        [84.85, 55.26],
        [68.86, 58.31],
    ],
    [[69.0, 59.34], [86.62, 58.04], [86.62, 61.96], [69.0, 60.66]],
    [
        [68.86, 61.69],
        [87.67, 65.28],
        [86.6, 69.26],
        [68.52, 62.97],
    ],
    [
        [68.12, 63.93],
        [86.75, 72.93],
        [84.57, 76.7],
        [67.46, 65.07],
    ],
    [[66.83, 65.9], [83.7, 80.46], [80.46, 83.7], [65.9, 66.83]],
    [
        [65.07, 67.46],
        [78.52, 87.26],
        [74.34, 89.67],
        [63.93, 68.12],
    ],
    [
        [62.97, 68.52],
        [71.4, 92.73],
        [66.49, 94.05],
        [61.69, 68.86],
    ],
    [[60.66, 69.0], [62.67, 96.31], [57.33, 96.31], [59.34, 69.0]],
    [
        [58.31, 68.86],
        [52.84, 97.53],
        [47.44, 96.08],
        [57.03, 68.52],
    ],
    [
        [56.07, 68.12],
        [42.57, 96.05],
        [37.5, 93.11],
        [54.93, 67.46],
    ],
    [[54.1, 66.83], [32.61, 91.73], [28.27, 87.39], [53.17, 65.9]],
    [
        [52.54, 65.07],
        [23.75, 84.63],
        [20.54, 79.07],
        [51.88, 63.93],
    ],
    [
        [51.48, 62.97],
        [16.77, 75.05],
        [15.04, 68.58],
        [51.14, 61.69],
    ],
    [[51.0, 60.66], [12.37, 63.5], [12.37, 56.5], [51.0, 59.34]],
    [
        [51.14, 58.31],
        [11.11, 50.67],
        [12.99, 43.63],
        [51.48, 57.03],
    ],
    [
        [51.88, 56.07],
        [13.34, 37.44],
        [17.14, 30.87],
        [52.54, 54.93],
    ],
    [[53.17, 54.1], [19.19, 24.78], [24.78, 19.19], [54.1, 53.17]],
    [
        [54.93, 52.54],
        [28.51, 13.67],
        [35.62, 9.57],
        [56.07, 51.88],
    ],
    [[56.88, 51.52], [39.89, 5.34], [50.09, 2.61], [58.46, 51.09]],
];

/// Derive beam centres and widths from the brand's four-point polygons.
pub(super) fn beam_geometry() -> &'static [Beam] {
    use std::sync::OnceLock;
    static GEOMETRY: OnceLock<Vec<Beam>> = OnceLock::new();
    GEOMETRY.get_or_init(|| {
        let mut beams = Vec::with_capacity(BEAM_POINTS.len());
        for points in BEAM_POINTS {
            let start = [
                (points[0][0] + points[3][0]) * 0.5,
                (points[0][1] + points[3][1]) * 0.5,
            ];
            let end = [
                (points[1][0] + points[2][0]) * 0.5,
                (points[1][1] + points[2][1]) * 0.5,
            ];
            let half_start = ((points[0][0] - points[3][0]).powi(2)
                + (points[0][1] - points[3][1]).powi(2))
            .sqrt()
                * 0.5;
            let half_end = ((points[1][0] - points[2][0]).powi(2)
                + (points[1][1] - points[2][1]).powi(2))
            .sqrt()
                * 0.5;
            let delta_x = end[0] - start[0];
            let delta_y = end[1] - start[1];
            let length = (delta_x * delta_x + delta_y * delta_y).sqrt().max(0.000_1);
            beams.push(Beam {
                points,
                start,
                end,
                half_start,
                half_end,
                normal_x: -delta_y / length,
                normal_y: delta_x / length,
            });
        }
        beams
    })
}

/// Convert a `usize` index to a float without an `as` cast.
///
/// Every caller's index is bounded by [`MAX_SHAPES`](crate::ink_lab::MAX_SHAPES) or
/// [`MAX_BEADS`](crate::ink_lab::MAX_BEADS), so the
/// conversion is exact; an index past `u32::MAX` saturates rather than
/// wrapping to a small value.
pub(super) fn index_to_float(index: usize) -> f64 {
    match u32::try_from(index) {
        Ok(value) => f64::from(value),
        Err(_) => f64::from(u32::MAX),
    }
}

/// Resolve an unset option to the effect's published brand default.
///
/// `Options` documents that unset fields take the brand value; this is that
/// rule in one place, not a fallback for a failed computation.
pub(super) fn brand_default<T>(value: Option<T>, brand: T) -> T {
    match value {
        Some(value) => value,
        None => brand,
    }
}

/// Insert thousands separators into already-formatted decimal digits.
pub(super) fn group_thousands(number: &str) -> String {
    let (integer, fraction) = match number.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (number, None),
    };
    let negative = integer.starts_with('-');
    let digits = match integer.strip_prefix('-') {
        Some(rest) => rest,
        None => integer,
    };
    let capacity = digits.len().wrapping_add(digits.len().wrapping_div(3));
    let mut grouped = String::with_capacity(capacity.wrapping_add(2));
    if negative {
        grouped.push('-');
    }
    let length = digits.len();
    for (index, character) in digits.chars().enumerate() {
        let remaining = length.wrapping_sub(index);
        if index != 0 && remaining.wrapping_rem(3) == 0 {
            grouped.push(',');
        }
        grouped.push(character);
    }
    if let Some(fraction) = fraction {
        grouped.push('.');
        grouped.push_str(fraction);
    }
    grouped
}
