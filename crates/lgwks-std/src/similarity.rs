//! Pure similarity primitives with replaceable scoring seams.
//!
//! This module keeps deterministic lexical, structural, and geometric scores
//! behind [`Similarity`]. Each concrete scorer owns only its policy; callers
//! can replace one scorer without changing the code that combines or consumes
//! scores. The algorithms use `core` and `alloc` only, and all returned scores
//! are clamped to the contract's closed interval `[0.0, 1.0]`.

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;
use core::marker::PhantomData;

/// A score-producing seam for one kind of comparable value.
pub trait Similarity {
    /// The value type accepted by this scorer.
    type Value: ?Sized;

    /// Scores how similar `left` and `right` are.
    ///
    /// Implementations return `1.0` for identical values and `0.0` for values
    /// with no similarity. Every result is finite and lies in `[0.0, 1.0]`.
    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64;
}

/// Why a bounded edit-distance calculation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditDistanceError {
    /// At least one input exceeded the scorer's declared character bound.
    InputTooLong {
        /// The largest permitted input length.
        maximum: usize,
        /// The first observed length known to exceed `maximum`.
        observed: usize,
    },
}

impl fmt::Display for EditDistanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InputTooLong { maximum, observed } => write!(
                formatter,
                "similarity input length {observed} exceeds maximum {maximum}"
            ),
        }
    }
}

impl core::error::Error for EditDistanceError {}

/// Bounded Levenshtein similarity over normalized text.
///
/// Inputs are trimmed, internal whitespace is collapsed, and Unicode
/// lower-case expansion is applied before the edit distance is calculated.
/// The score is `1 - distance / max(input lengths)`, with two empty inputs
/// scoring `1.0`. The dynamic-programming row is bounded by
/// `maximum_length`, so hostile input is refused before the row is allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EditDistance {
    /// The maximum raw character count accepted per input.
    maximum_length: usize,
}

impl EditDistance {
    /// Creates a scorer that accepts at most `maximum_length` input characters.
    #[must_use]
    pub const fn new(maximum_length: usize) -> Self {
        Self { maximum_length }
    }

    /// Returns the maximum number of raw input characters accepted per side.
    #[must_use]
    pub const fn maximum_length(&self) -> usize {
        self.maximum_length
    }

    /// Calculates the normalized score or reports an over-limit input.
    pub fn try_score(&self, left: &str, right: &str) -> Result<f64, EditDistanceError> {
        let left = normalize_text(left, self.maximum_length)?;
        let right = normalize_text(right, self.maximum_length)?;
        Ok(sequence_similarity(&left, &right))
    }
}

impl Similarity for EditDistance {
    type Value = str;

    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        self.try_score(left, right).unwrap_or(0.0)
    }
}

/// A set-based Jaccard similarity scorer.
///
/// `T` is intentionally constrained only by equality. The implementation
/// treats duplicate entries as one set member and uses bounded pairwise scans,
/// so it needs no hashing policy or third-party dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Jaccard<T = String> {
    /// Keeps the collection element type in the scorer's type identity.
    marker: PhantomData<fn() -> T>,
}

impl<T> Jaccard<T> {
    /// Creates a Jaccard scorer for collections of `T`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            marker: PhantomData,
        }
    }
}

impl<T> Default for Jaccard<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: PartialEq> Similarity for Jaccard<T> {
    type Value = [T];

    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        if left.is_empty() && right.is_empty() {
            return 1.0;
        }
        if left.is_empty() || right.is_empty() {
            return 0.0;
        }

        let left_unique = unique_count(left);
        let right_unique = unique_count(right);
        let intersection = left
            .iter()
            .enumerate()
            .filter(|item| {
                let index = item.0;
                let value = item.1;
                !left[..index].iter().any(|previous| previous == value)
                    && right.iter().any(|candidate| candidate == value)
            })
            .fold(0.0, |count, _| count + 1.0);
        let union = left_unique + right_unique - intersection;
        unit_score(intersection / union)
    }
}

/// Structural path similarity that ignores volatile identifiers and hashes.
///
/// Paths may use `/`, `>`, or whitespace as separators. Numeric segments and
/// generated-looking class/hash suffixes are discarded before a normalized
/// Levenshtein comparison of the remaining structural segments.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PathSimilarity;

impl PathSimilarity {
    /// Creates a structural path scorer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Similarity for PathSimilarity {
    type Value = str;

    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        let left = normalized_path(left);
        let right = normalized_path(right);
        sequence_similarity(&left, &right)
    }
}

/// A normalized four-coordinate bounding box.
///
/// Coordinates are caller-defined normalized values, commonly fractions of a
/// viewport. No coordinate is silently clipped; [`Geometry`] handles
/// non-finite values by returning the safe lower score.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct BoundingBox {
    /// The normalized horizontal origin.
    x: f64,
    /// The normalized vertical origin.
    y: f64,
    /// The normalized width.
    width: f64,
    /// The normalized height.
    height: f64,
}

impl BoundingBox {
    /// Creates a bounding box from its normalized origin and dimensions.
    #[must_use]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns the normalized horizontal origin.
    #[must_use]
    pub const fn x(&self) -> f64 {
        self.x
    }

    /// Returns the normalized vertical origin.
    #[must_use]
    pub const fn y(&self) -> f64 {
        self.y
    }

    /// Returns the normalized width.
    #[must_use]
    pub const fn width(&self) -> f64 {
        self.width
    }

    /// Returns the normalized height.
    #[must_use]
    pub const fn height(&self) -> f64 {
        self.height
    }

    /// Returns coordinates in `(x, y, width, height)` order.
    #[must_use]
    pub const fn as_array(&self) -> [f64; 4] {
        [self.x, self.y, self.width, self.height]
    }
}

impl From<[f64; 4]> for BoundingBox {
    fn from(values: [f64; 4]) -> Self {
        Self::new(values[0], values[1], values[2], values[3])
    }
}

/// Geometric similarity over normalized bounding boxes.
///
/// The Euclidean distance between the four coordinates is mapped with
/// `1 - distance / maximum_distance` and clamped to `[0.0, 1.0]`. A
/// non-positive or non-finite maximum is treated as an exact-match-only
/// policy, which keeps construction infallible and every score bounded.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Geometry {
    /// The caller-supplied distance that maps to zero similarity.
    maximum_distance: f64,
}

impl Geometry {
    /// Creates a geometry scorer with the caller-supplied maximum distance.
    #[must_use]
    pub const fn new(maximum_distance: f64) -> Self {
        Self { maximum_distance }
    }

    /// Returns the maximum distance used for normalization.
    #[must_use]
    pub const fn maximum_distance(&self) -> f64 {
        self.maximum_distance
    }

    /// Scores two equal-shaped bounding-box values.
    ///
    /// The inherent method accepts either a `[f64; 4]` or a [`BoundingBox`].
    /// The [`Similarity`] implementation below uses `[f64; 4]` as the stable
    /// trait value, so the scorer can also be placed in [`Weighted`].
    #[must_use]
    pub fn score<Value>(&self, left: &Value, right: &Value) -> f64
    where
        Value: Copy + Into<[f64; 4]>,
    {
        let left = (*left).into();
        let right = (*right).into();
        geometry_score(self.maximum_distance, &left, &right)
    }
}

impl Similarity for Geometry {
    type Value = [f64; 4];

    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        geometry_score(self.maximum_distance, left, right)
    }
}

/// Why a weighted scorer could not be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WeightedError {
    /// No component scorer was supplied.
    Empty,
    /// A component weight was negative or non-finite.
    InvalidWeight {
        /// Index of the invalid component.
        index: usize,
    },
    /// The component weights would produce a score above `1.0`.
    WeightSumExceedsOne,
    /// The acceptance threshold was outside the finite unit interval.
    InvalidThreshold,
}

impl fmt::Display for WeightedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Empty => formatter.write_str("weighted similarity needs a component"),
            Self::InvalidWeight { index } => {
                write!(
                    formatter,
                    "weighted similarity has invalid weight at {index}"
                )
            }
            Self::WeightSumExceedsOne => {
                formatter.write_str("weighted similarity weights must sum to at most 1.0")
            }
            Self::InvalidThreshold => {
                formatter.write_str("weighted similarity threshold must be in [0.0, 1.0]")
            }
        }
    }
}

impl core::error::Error for WeightedError {}

/// A weighted composition of replaceable similarity scorers.
///
/// Components are evaluated as `sum(weight * score)`. Construction rejects an
/// empty component set, invalid weights, and totals above `1.0`, preserving the
/// range promised by [`Similarity`].
pub struct Weighted<Value: ?Sized> {
    /// Non-empty scorers and their bounded non-negative weights.
    components: Vec<(f64, Box<dyn Similarity<Value = Value>>)>,
    /// The score at or above which [`Self::is_accepted`] returns `true`.
    threshold: f64,
}

impl<Value: ?Sized> Weighted<Value> {
    /// Creates a weighted scorer and its acceptance threshold.
    pub fn new(
        components: Vec<(f64, Box<dyn Similarity<Value = Value>>)>,
        threshold: f64,
    ) -> Result<Self, WeightedError> {
        if components.is_empty() {
            return Err(WeightedError::Empty);
        }
        if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
            return Err(WeightedError::InvalidThreshold);
        }

        let mut total = 0.0;
        for (index, component) in components.iter().enumerate() {
            let weight = component.0;
            if !weight.is_finite() || weight < 0.0 {
                return Err(WeightedError::InvalidWeight { index });
            }
            total += weight;
        }
        if total > 1.0 {
            return Err(WeightedError::WeightSumExceedsOne);
        }

        Ok(Self {
            components,
            threshold,
        })
    }

    /// Alias for [`Weighted::new`] when a fallible constructor name is clearer
    /// at a call site.
    pub fn try_new(
        components: Vec<(f64, Box<dyn Similarity<Value = Value>>)>,
        threshold: f64,
    ) -> Result<Self, WeightedError> {
        Self::new(components, threshold)
    }

    /// Returns the acceptance threshold.
    #[must_use]
    pub const fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Scores the pair and reports whether it reaches the configured threshold.
    #[must_use]
    pub fn is_accepted(&self, left: &Value, right: &Value) -> bool {
        self.score(left, right) >= self.threshold
    }
}

impl<Value: ?Sized> Similarity for Weighted<Value> {
    type Value = Value;

    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        let total = self.components.iter().fold(0.0, |sum, component| {
            sum + (component.0 * unit_score(component.1.score(left, right)))
        });
        unit_score(total)
    }
}

/// Converts a finite candidate into the similarity contract's unit interval.
fn unit_score(candidate: f64) -> f64 {
    if candidate.is_finite() {
        candidate.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Normalizes text without allocating until the raw bound has been checked.
fn normalize_text(input: &str, maximum: usize) -> Result<Vec<char>, EditDistanceError> {
    let mut observed = 0usize;
    for _ in input.chars() {
        observed = observed.saturating_add(1);
        if observed > maximum {
            return Err(EditDistanceError::InputTooLong { maximum, observed });
        }
    }

    let mut normalized = Vec::with_capacity(observed);
    let mut pending_space = false;
    for character in input.chars() {
        if character.is_whitespace() {
            if !normalized.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if pending_space {
            normalized.push(' ');
            pending_space = false;
        }
        for lower in character.to_lowercase() {
            normalized.push(lower);
        }
    }

    if normalized.len() > maximum {
        return Err(EditDistanceError::InputTooLong {
            maximum,
            observed: normalized.len(),
        });
    }
    Ok(normalized)
}

/// Computes normalized Levenshtein similarity with one rolling row.
fn sequence_similarity<T: PartialEq>(left: &[T], right: &[T]) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }

    let (rows, columns) = if left.len() < right.len() {
        (left, right)
    } else {
        (right, left)
    };

    let mut previous: Vec<f64> = Vec::with_capacity(columns.len().saturating_add(1));
    let mut denominator = 0.0f64;
    previous.push(0.0);
    for _ in columns {
        denominator += 1.0;
        previous.push(denominator);
    }
    let mut current: Vec<f64> = vec![0.0; columns.len().saturating_add(1)];
    let mut row_distance = 1.0;
    for row_value in rows {
        current[0] = row_distance;
        for (column_index, column_value) in columns.iter().enumerate() {
            let cost = if row_value == column_value { 0.0 } else { 1.0 };
            let index = column_index.saturating_add(1);
            let deletion = previous[index] + 1.0;
            let insertion = current[column_index] + 1.0;
            let substitution = previous[column_index] + cost;
            current[index] = deletion.min(insertion).min(substitution);
        }
        core::mem::swap(&mut previous, &mut current);
        row_distance += 1.0;
    }

    unit_score(1.0 - (previous[columns.len()] / denominator))
}

/// Counts distinct values without requiring a hashing policy.
fn unique_count<T: PartialEq>(values: &[T]) -> f64 {
    values
        .iter()
        .enumerate()
        .filter(|item| {
            let index = item.0;
            let value = item.1;
            !values[..index].iter().any(|previous| previous == value)
        })
        .fold(0.0, |count, _| count + 1.0)
}

/// Removes path syntax and volatile segments while borrowing the input.
fn normalized_path(path: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    for part in path.split(|character: char| {
        character == '/' || character == '\\' || character == '>' || character.is_whitespace()
    }) {
        for component in part.split(['#', '.']) {
            let component = component.trim();
            if !component.is_empty() && !is_volatile_segment(component) {
                segments.push(component);
            }
        }
    }
    segments
}

/// Recognizes numeric ids and common generated class/hash suffixes.
fn is_volatile_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if bytes.is_empty() || bytes.iter().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    if bytes.len() >= 6 && bytes.iter().all(|byte| byte.is_ascii_hexdigit()) {
        return true;
    }

    let suffix_is_numeric =
        |suffix: &str| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit());
    let suffix_is_hash = |suffix: &str| {
        suffix.len() >= 6
            && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
            && suffix.bytes().any(|byte| byte.is_ascii_digit())
    };
    if let Some((_, suffix)) = segment.rsplit_once(['-', '_'])
        && (suffix_is_numeric(suffix) || suffix_is_hash(suffix))
    {
        return true;
    }
    ["css-", "sc-", "jsx-", "emotion-", "hash-"]
        .iter()
        .any(|prefix| {
            segment.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() >= 4 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
        })
}

/// Computes normalized Euclidean similarity for four coordinates.
fn geometry_score(maximum_distance: f64, left: &[f64; 4], right: &[f64; 4]) -> f64 {
    let mut squared_distance = 0.0;
    for (left_value, right_value) in left.iter().zip(right.iter()) {
        let difference = *left_value - *right_value;
        if !difference.is_finite() {
            return 0.0;
        }
        squared_distance = difference.mul_add(difference, squared_distance);
        if !squared_distance.is_finite() {
            return 0.0;
        }
    }
    let distance = squared_distance.sqrt();
    if !distance.is_finite() {
        return 0.0;
    }
    if maximum_distance.is_finite() && maximum_distance > 0.0 {
        unit_score(1.0 - (distance / maximum_distance))
    } else if distance <= f64::EPSILON {
        1.0
    } else {
        0.0
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_unit(value: f64) {
        assert!(value.is_finite(), "similarity must be finite: {value}");
        assert!(
            (0.0..=1.0).contains(&value),
            "similarity outside [0, 1]: {value}"
        );
    }

    /// Compares floating-point outputs without relying on exact arithmetic.
    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-12,
            "expected {expected}, got {actual}"
        );
    }

    /// Erases one edit scorer for a weighted composition without a cast.
    fn boxed_edit_distance(maximum_length: usize) -> Box<dyn Similarity<Value = str>> {
        Box::new(EditDistance::new(maximum_length))
    }

    #[test]
    fn edit_distance_handles_empty_and_identical_inputs() -> Result<(), EditDistanceError> {
        let scorer = EditDistance::new(16);
        assert_close(scorer.try_score("", "")?, 1.0);
        assert_close(scorer.try_score("same", "same")?, 1.0);
        assert_close(scorer.try_score("", "text")?, 0.0);
        assert_close(scorer.try_score("text", "")?, 0.0);
        assert_close(scorer.try_score("  SAME ", "same")?, 1.0);
        Ok(())
    }

    #[test]
    fn edit_distance_normalizes_known_distance() -> Result<(), EditDistanceError> {
        let scorer = EditDistance::new(16);
        let score = scorer.try_score("kitten", "sitting")?;
        assert!((score - (4.0 / 7.0)).abs() < 1e-12, "score was {score}");
        Ok(())
    }

    #[test]
    fn edit_distance_refuses_over_limit_inputs() -> Result<(), EditDistanceError> {
        let scorer = EditDistance::new(3);
        assert_close(scorer.try_score("abc", "abc")?, 1.0);
        assert!(matches!(
            scorer.try_score("four", "four"),
            Err(EditDistanceError::InputTooLong {
                maximum: 3,
                observed: 4
            })
        ));
        assert_close(scorer.score("four", "four"), 0.0);
        assert!(
            scorer.score("four", "four") <= 0.0,
            "the infallible trait method must map refusal to no similarity"
        );
        Ok(())
    }

    #[test]
    fn jaccard_handles_boundary_sets() {
        let scorer = Jaccard::<&str>::new();
        assert_close(scorer.score(&[], &[]), 1.0);
        assert_close(scorer.score(&["a", "b"], &["a", "b"]), 1.0);
        assert_close(scorer.score(&["a"], &["b"]), 0.0);
        assert_close(scorer.score(&["a", "a"], &["a"]), 1.0);
    }

    #[test]
    fn path_similarity_discards_volatile_segments() {
        let scorer = PathSimilarity::new();
        assert_close(
            scorer.score(
                "html/body/div/123/button.css-a1b2c3",
                "html/body/div/987/button.css-z9y8x7",
            ),
            1.0,
        );
        assert_close(scorer.score("root/left", "root/right"), 0.5);
        assert_close(scorer.score("", ""), 1.0);
        assert_close(scorer.score("left", "right"), 0.0);
    }

    #[test]
    fn geometry_maps_zero_and_maximum_distance() {
        let scorer = Geometry::new(2.0);
        let origin = [0.0, 0.0, 0.0, 0.0];
        let far = [2.0, 0.0, 0.0, 0.0];
        assert_close(scorer.score(&origin, &origin), 1.0);
        assert_close(scorer.score(&origin, &far), 0.0);
        assert_close(scorer.score(&origin, &[1.0, 0.0, 0.0, 0.0]), 0.5);
    }

    #[test]
    fn bounding_box_accessors_preserve_coordinates() {
        let box_value = BoundingBox::new(0.1, 0.2, 0.3, 0.4);
        assert_close(box_value.x(), 0.1);
        assert_close(box_value.y(), 0.2);
        assert_close(box_value.width(), 0.3);
        assert_close(box_value.height(), 0.4);
        for (actual, expected) in box_value.as_array().iter().zip([0.1, 0.2, 0.3, 0.4]) {
            assert_close(*actual, expected);
        }
    }

    #[test]
    fn weighted_rejects_empty_components() {
        let result = Weighted::<str>::new(Vec::new(), 0.5);
        assert!(matches!(result, Err(WeightedError::Empty)));
    }

    #[test]
    fn weighted_single_vector_returns_that_vectors_score() -> Result<(), WeightedError> {
        let direct = EditDistance::new(16);
        let weighted = Weighted::<str>::new(vec![(1.0, boxed_edit_distance(16))], 0.8)?;
        assert_close(
            weighted.score("kitten", "sitting"),
            direct.score("kitten", "sitting"),
        );
        assert!(!weighted.is_accepted("kitten", "sitting"));
        Ok(())
    }

    #[test]
    fn every_metric_stays_in_unit_interval_for_fixed_inputs() -> Result<(), WeightedError> {
        let edit = EditDistance::new(32);
        for (left, right) in [("", ""), ("a", "b"), ("short", "longer"), ("A B", "a b")] {
            assert_unit(edit.score(left, right));
        }

        let sets = Jaccard::<&str>::new();
        for (left, right) in [
            (&[][..], &[][..]),
            (&["a"][..], &["b"][..]),
            (&["a", "b"][..], &["b", "c"][..]),
        ] {
            assert_unit(sets.score(left, right));
        }

        let paths = PathSimilarity::new();
        for (left, right) in [("", ""), ("root/1", "root/2"), ("a", "b/c")] {
            assert_unit(paths.score(left, right));
        }

        let geometry = Geometry::new(1.0);
        for (left, right) in [
            ([0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]),
            ([0.0, 0.0, 0.0, 0.0], [1.0, 1.0, 1.0, 1.0]),
            ([0.5, 0.5, 0.25, 0.25], [0.25, 0.25, 0.5, 0.5]),
        ] {
            assert_unit(geometry.score(&left, &right));
        }

        let weighted = Weighted::<str>::new(vec![(1.0, boxed_edit_distance(32))], 0.5)?;
        for (left, right) in [
            ("", ""),
            ("same", "same"),
            ("a", "b"),
            ("too long", "too long"),
        ] {
            assert_unit(weighted.score(left, right));
        }
        Ok(())
    }
}
