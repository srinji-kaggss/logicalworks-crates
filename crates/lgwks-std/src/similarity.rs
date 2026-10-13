//! Pure similarity primitives with replaceable scoring seams.
//!
//! This module keeps deterministic lexical, structural, and geometric scores
//! behind [`Similarity`]. Each concrete scorer owns only its policy; callers
//! can replace one scorer without changing the code that combines or consumes
//! scores. The algorithms use `core` and `alloc` only, and every score the
//! [`Similarity`] seam returns is clamped to the contract's closed interval
//! `[0.0, 1.0]`.
//!
//! # Two domains, and the difference between a measurement and a refusal
//!
//! Raw cosine similarity has a mathematical domain of `[-1, 1]`, and
//! [`Cosine::try_score`] keeps it. The shared [`Similarity`] contract has the
//! domain `[0, 1]`, and the mapping between the two is the explicit, named,
//! versioned [`Cosine::normalized_score`] (`(raw + 1) / 2`). An opposite pair
//! is therefore `0.0` through the shared contract and `-1.0` through the raw
//! one, which is a stated mapping rather than a silent shift.
//!
//! A weight total below `1.0` is a deliberate evidence deficit, so an identical
//! pair scored at weight `0.5` reads `0.5` rather than the trait's identity
//! `1.0`. The maximum is not renormalized to hide the deficit, and the deficit
//! is never presented as an identity score.
//!
//! A refusal is not a measurement. Every checked scorer reports one through a
//! typed error, and [`CheckedEvidence::verdict`] carries component identity,
//! the refusal, and applicability through to the acceptance decision, so an
//! unavailable component cannot satisfy a threshold — including a threshold of
//! zero. All-zero effective evidence is [`EvidenceError::InsufficientEvidence`]
//! rather than a score of zero.
//!
//! The infallible [`Similarity`] methods remain for source compatibility and
//! are documented as lossy: they map a refusal to `0.0`. They are not the
//! authority-facing path; the checked composition is.
//!
//! [`Similarity`]: crate::similarity::Similarity
//! [`Cosine::try_score`]: crate::similarity::Cosine::try_score
//! [`Cosine::normalized_score`]: crate::similarity::Cosine::normalized_score
//! [`CheckedEvidence::verdict`]: crate::similarity::CheckedEvidence::verdict
//! [`EvidenceError::InsufficientEvidence`]: crate::similarity::EvidenceError::InsufficientEvidence
//!
//! # Example
//!
//! ```rust
//! use lgwks_std::similarity::{Cosine, CosineError, Similarity};
//!
//! let cosine = Cosine::new();
//! assert_eq!(cosine.try_score(&[1.0_f32], &[-1.0_f32])?, -1.0);
//! assert_eq!(Similarity::score(&cosine, &[1.0_f32], &[-1.0_f32]), 0.0);
//! assert!(matches!(
//!     cosine.try_score(&[1.0_f32, 0.0], &[1.0_f32, 0.0, 0.0]),
//!     Err(CosineError::DimensionMismatch { .. })
//! ));
//! # Ok::<(), CosineError>(())
//! ```
//!
//! The named normalized mapping is available on its own:
//!
//! ```rust
//! use lgwks_std::similarity::Cosine;
//!
//! let cosine = Cosine::new();
//! // Opposite reads 0.0 through the shared contract.
//! assert_eq!(cosine.normalized_score(&[1.0_f32], &[-1.0_f32]).unwrap_or(-1.0), 0.0);
//! ```

extern crate alloc;

use alloc::string::String;
use core::fmt;
use core::marker::PhantomData;

/// A score-producing seam for one kind of comparable value.
///
/// Every implementation returns `1.0` for identical values, `0.0` for values
/// with no similarity, and a finite result in `[0.0, 1.0]`. Implementations
/// that can refuse document the infallible mapping in their own `impl` block;
/// [`CheckedEvidence::verdict`] is the path that preserves the refusal.
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

impl From<EditDistanceError> for EvidenceError {
    fn from(value: EditDistanceError) -> Self {
        match value {
            EditDistanceError::InputTooLong { maximum, observed } => {
                Self::InputTooLong { maximum, observed }
            }
        }
    }
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

    /// Returns the normalized scalar count an input occupies under this
    /// scorer's text normalization.
    ///
    /// Normalization trims, collapses internal whitespace, and applies Unicode
    /// lower-case expansion, so this is the unit [`Self::maximum_length`]
    /// charges rather than the raw scalar count. Lower-casing can expand a
    /// scalar — `İ` becomes two — so the two units differ and only this one is
    /// the budget.
    #[must_use]
    pub fn normalized_length(&self, input: &str) -> usize {
        // Under a `usize::MAX` budget the refusal cannot fire; were it to, it
        // carries the count that crossed the bound, which is still the answer.
        match normalize_text(input, usize::MAX) {
            Ok(normalized) => normalized.len(),
            Err(EditDistanceError::InputTooLong { observed, .. }) => observed,
        }
    }

    /// Calculates the normalized score or reports an over-limit input.
    ///
    /// The budget is charged against the normalized scalar count, so a scalar
    /// whose lower-case form expands can be refused even when its raw scalar
    /// count is inside the limit. The refusal names the normalized length that
    /// crossed the bound.
    ///
    /// # Errors
    ///
    /// Returns [`EditDistanceError::InputTooLong`] when either input's
    /// normalized length exceeds [`Self::maximum_length`]. Both the raw pass
    /// and the normalized pass are checked, and the raw pass runs first, so a
    /// hostile input is refused before the row is allocated.
    pub fn try_score(&self, left: &str, right: &str) -> Result<f64, EditDistanceError> {
        let left = normalize_text(left, self.maximum_length)?;
        let right = normalize_text(right, self.maximum_length)?;
        Ok(sequence_similarity(&left, &right))
    }
}

/// Why a checked comparison could not produce a measurement.
///
/// Every variant is a refusal, never a score. A refusal reaching a composition
/// is retained with its component identity and can never satisfy an acceptance
/// threshold, so "we could not measure this" and "these did not match" stay
/// distinguishable through every layer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvidenceError {
    /// The two vectors had different lengths.
    DimensionMismatch {
        /// Length of the left-hand vector.
        left: usize,
        /// Length of the right-hand vector.
        right: usize,
    },
    /// A vector had zero magnitude, so its angle is undefined.
    ZeroMagnitude,
    /// A vector or coordinate carried a value that is not finite.
    NonFinite,
    /// At least one input exceeded the scorer's declared normalized budget.
    InputTooLong {
        /// The largest permitted normalized length.
        maximum: usize,
        /// The first normalized length known to exceed `maximum`.
        observed: usize,
    },
    /// A collection exceeded the scorer's declared element budget.
    CollectionTooLong {
        /// The largest permitted element count.
        maximum: usize,
        /// The first observed count known to exceed `maximum`.
        observed: usize,
    },
    /// The composition had no effective evidence at all.
    ///
    /// A weight total of zero is not a measurement of zero similarity; it is
    /// the absence of anything to weigh, and it refuses regardless of the
    /// configured threshold.
    InsufficientEvidence {
        /// The sum of the configured component weights.
        total_weight: u64,
    },
    /// The weighted composition itself could not be constructed or evaluated.
    Composition(WeightedError),
}

impl fmt::Display for EvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::DimensionMismatch { left, right } => write!(
                formatter,
                "cosine vectors differ in length: {left} and {right}"
            ),
            Self::ZeroMagnitude => formatter.write_str("cosine vector has zero magnitude"),
            Self::NonFinite => formatter.write_str("cosine vector is not finite"),
            Self::InputTooLong { maximum, observed } => write!(
                formatter,
                "similarity input length {observed} exceeds maximum {maximum}"
            ),
            Self::CollectionTooLong { maximum, observed } => write!(
                formatter,
                "similarity collection length {observed} exceeds maximum {maximum}"
            ),
            Self::InsufficientEvidence { total_weight } => write!(
                formatter,
                "weighted similarity has no effective evidence (total weight {total_weight})"
            ),
            Self::Composition(inner) => {
                write!(formatter, "weighted similarity composition: {inner}")
            }
        }
    }
}

impl core::error::Error for EvidenceError {}

impl From<WeightedError> for EvidenceError {
    fn from(value: WeightedError) -> Self {
        Self::Composition(value)
    }
}

impl From<CosineError> for EvidenceError {
    fn from(value: CosineError) -> Self {
        match value {
            CosineError::DimensionMismatch { left, right } => {
                Self::DimensionMismatch { left, right }
            }
            CosineError::ZeroMagnitude => Self::ZeroMagnitude,
            CosineError::NonFinite => Self::NonFinite,
        }
    }
}

/// One component's outcome inside a checked composition.
///
/// The fields are private because an outcome is a report about a measurement,
/// not a value the caller may rewrite: a caller that could set `score` to
/// `Some(1.0)` on a refused component would defeat the whole contract.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ComponentOutcome {
    /// Position of this component in the composition's weight list.
    index: usize,
    /// The component's configured weight.
    weight: f64,
    /// The measured score, or [`None`] when the component refused.
    score: Option<f64>,
    /// Why this component refused, or [`None`] when it measured.
    reason: Option<EvidenceError>,
}

impl ComponentOutcome {
    /// Position of this component in the composition's weight list.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// The component's configured weight.
    #[must_use]
    pub const fn weight(&self) -> f64 {
        self.weight
    }

    /// The measured score, or [`None`] when the component refused.
    ///
    /// [`None`] is the refusal signal, distinct from a measured `0.0`.
    #[must_use]
    pub const fn score(&self) -> Option<f64> {
        self.score
    }

    /// Why this component refused, or [`None`] when it measured.
    #[must_use]
    pub const fn reason(&self) -> Option<&EvidenceError> {
        self.reason.as_ref()
    }

    /// Whether this component produced a usable measurement.
    ///
    /// A refused component is never applicable, so it cannot contribute to an
    /// acceptance decision at any threshold.
    #[must_use]
    pub const fn is_applicable(&self) -> bool {
        self.score.is_some() && self.reason.is_none()
    }
}

/// One component's evidence: either a measurement or a refusal.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Evidence {
    /// A finite measured score.
    Measured {
        /// The measured score.
        value: f64,
    },
    /// The component could not be measured.
    Refused {
        /// Why it could not be measured.
        reason: EvidenceError,
    },
}

impl Evidence {
    /// Wraps a measured score as applicable evidence.
    #[must_use]
    pub const fn measured(value: f64) -> Self {
        Self::Measured { value }
    }

    /// Wraps a refusal as inapplicable evidence.
    #[must_use]
    pub const fn refused(reason: EvidenceError) -> Self {
        Self::Refused { reason }
    }

    /// Whether this evidence can be composed.
    ///
    /// A refusal is not applicable: unavailable evidence never acquires the
    /// authority of a measurement.
    #[must_use]
    pub const fn is_applicable(&self) -> bool {
        matches!(self, Self::Measured { .. })
    }

    /// The measured score, or [`None`] for a refusal.
    #[must_use]
    pub const fn value(&self) -> Option<f64> {
        match *self {
            Self::Measured { value } => Some(value),
            Self::Refused { .. } => None,
        }
    }
}

/// The `Similarity` reading of one checked score, for the lossy infallible path.
///
/// `Similarity::score` returns an `f64` and so cannot report why a scorer
/// refused: a missing edit-length budget, an input past a set's ceiling or a
/// zero-norm cosine all read as `0.0` there. That loss is the compatibility
/// contract INV-STD-SIM-2 keeps, and it is stated in one place rather than
/// repeated per scorer: the refusal is emitted with the scorer's name, so a
/// caller that reads a `0.0` off this path can see that it was one. The
/// authority-facing path is [`CheckedSimilarity`], which returns the refusal.
fn lossy_score<E: std::fmt::Debug>(scored: Result<f64, E>, scorer: &'static str) -> f64 {
    match scored {
        Ok(score) => score,
        Err(refusal) => {
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal, scorer = scorer, "Similarity::score: a refusal reads as zero on the lossy path");
            #[cfg(not(feature = "trace"))]
            let _ = (scorer, refusal);
            0.0
        }
    }
}

impl Similarity for EditDistance {
    type Value = str;

    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        lossy_score(self.try_score(left, right), "EditDistance")
    }
}

/// A set-based Jaccard similarity scorer with no length ceiling.
///
/// `T` is intentionally constrained only by equality. The implementation
/// treats duplicate entries as one set member and uses bounded pairwise scans,
/// so it needs no hashing policy or third-party dependency.
///
/// The pairwise scans are `O(n²)` in the element count and this scorer declares
/// no ceiling, so it is reachable from the default `core` build with no bound at
/// all on caller-supplied input. That is stated here rather than left to be
/// discovered: prefer [`BoundedJaccard`] whenever the input comes from outside
/// the process, and use this type for small in-process sets whose size you
/// control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Jaccard<T = String> {
    /// Keeps the collection element type in the scorer's type identity.
    marker: PhantomData<fn() -> T>,
}

impl<T> Jaccard<T> {
    /// Creates an unbounded Jaccard scorer for collections of `T`.
    ///
    /// For untrusted input use [`BoundedJaccard::new`], which refuses before
    /// the quadratic scan rather than after it.
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

/// A Jaccard scorer that refuses an over-budget element count.
///
/// The budget is charged against the *input* length, before dedup and before
/// the pairwise scans, so a hostile input is refused without paying the `O(n²)`
/// cost it was trying to cause. The refusal names the limit and the observed
/// count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BoundedJaccard<T = String> {
    /// The largest element count accepted per side.
    maximum_length: usize,
    /// Keeps the collection element type in the scorer's type identity.
    marker: PhantomData<fn() -> T>,
}

impl<T> BoundedJaccard<T> {
    /// Creates a scorer that accepts at most `maximum_length` elements per side.
    #[must_use]
    pub const fn new(maximum_length: usize) -> Self {
        Self {
            maximum_length,
            marker: PhantomData,
        }
    }

    /// Returns the maximum element count accepted per side.
    #[must_use]
    pub const fn maximum_length(&self) -> usize {
        self.maximum_length
    }
}

impl<T> Default for BoundedJaccard<T> {
    fn default() -> Self {
        Self::new(256)
    }
}

/// Scores two sets, or refuses an over-budget element count.
///
/// The empty/empty, empty/non-empty, and full-overlap cases are handled before
/// the scans, so the budget is charged against what the caller supplied rather
/// than against dedup work.
fn bounded_jaccard_score<T: PartialEq>(
    maximum_length: usize,
    left: &[T],
    right: &[T],
) -> Result<f64, EvidenceError> {
    let observed = left.len().max(right.len());
    if observed > maximum_length {
        let refusal = Err(EvidenceError::CollectionTooLong {
            maximum: maximum_length,
            observed,
        });
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "bounded_jaccard_score: returning an error to the caller");
        return refusal;
    }
    Ok(jaccard_unit(left, right))
}

impl<T: PartialEq + Sync> CheckedSimilarity for BoundedJaccard<T> {
    type Value = [T];

    fn try_score(&self, left: &Self::Value, right: &Self::Value) -> Result<f64, EvidenceError> {
        bounded_jaccard_score(self.maximum_length, left, right)
    }
}

impl<T: PartialEq> Similarity for BoundedJaccard<T> {
    type Value = [T];

    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        lossy_score(
            bounded_jaccard_score(self.maximum_length, left, right),
            "BoundedJaccard",
        )
    }
}

impl<T: PartialEq> Similarity for Jaccard<T> {
    type Value = [T];

    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        jaccard_unit(left, right)
    }
}

/// Scores two collections as sets, with no input ceiling.
///
/// `PartialEq` is the only law used, so `T` must have equivalence-like
/// equality: a type whose equality is not reflexive — `f64` with `NaN`, say —
/// does not satisfy set-identity laws and is outside the supported domain.
/// Callers scoring floats should map them to an ordered key first.
fn jaccard_unit<T: PartialEq>(left: &[T], right: &[T]) -> f64 {
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

/// Whether two paths are exactly equal, with no normalization applied.
///
/// [`PathSimilarity`] deliberately drops numeric, hash-suffixed and
/// class-prefixed segments, so two genuinely different paths can score `1.0`.
/// That makes the score a structural heuristic, not an identity claim, and this
/// is the exact comparison a caller reaches for when it needs one. Never
/// substitute a heuristic score for this, and never use this as a canonical
/// key: a path that differs only in a dropped segment is a different path.
#[must_use]
pub fn is_exact_path_match(left: &str, right: &str) -> bool {
    left == right
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
    ///
    /// Named `to_` rather than `as_` because it returns an owned array by value:
    /// the four coordinates are separate fields, so there is no contiguous
    /// array behind `&self` to borrow from, and the conversion therefore costs
    /// a copy rather than a reborrow.
    #[must_use]
    pub const fn to_array(&self) -> [f64; 4] {
        [self.x, self.y, self.width, self.height]
    }
}

impl From<[f64; 4]> for BoundingBox {
    fn from(values: [f64; 4]) -> Self {
        Self::new(values[0], values[1], values[2], values[3])
    }
}

impl From<BoundingBox> for [f64; 4] {
    fn from(value: BoundingBox) -> Self {
        value.to_array()
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

/// Why a cosine similarity was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CosineError {
    /// The two vectors had different lengths.
    ///
    /// A dimension mismatch is a defect in whatever produced the vectors, not a
    /// pair of values that merely differ, so it is reported rather than scored
    /// as zero. Reporting `0.0` would make an inconsistent provider
    /// indistinguishable from two genuinely unrelated vectors.
    DimensionMismatch {
        /// Length of the left-hand vector.
        left: usize,
        /// Length of the right-hand vector.
        right: usize,
    },
    /// A vector had zero magnitude, so its angle is undefined.
    ///
    /// An all-zero vector is a direction that does not exist. It is reported
    /// apart from [`Self::NonFinite`] because the two have different causes and
    /// different repairs: a zero vector is usually an empty or padded input at
    /// the caller, where a non-finite one is a numeric fault inside whatever
    /// produced it.
    ZeroMagnitude,
    /// A vector carried a value that is not finite, so no angle can be computed.
    ///
    /// This is a *fourth* state behind a metric that most callers read as one
    /// number, and it needs to be its own variant rather than folded into
    /// [`Self::ZeroMagnitude`]. `NaN` compares `false` against everything,
    /// including itself, so a caller that lets one through a comparison reads
    /// "no measurement was possible" as "the measurement came out negative" —
    /// which is exactly the inference that must not be available. The
    /// accumulated sums are checked as well as the inputs, because a vector of
    /// finite values can still overflow to infinity.
    NonFinite,
}

impl fmt::Display for CosineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::DimensionMismatch { left, right } => write!(
                formatter,
                "cosine vectors differ in length: {left} and {right}"
            ),
            Self::ZeroMagnitude => formatter.write_str("cosine vector has zero magnitude"),
            Self::NonFinite => formatter.write_str("cosine vector is not finite"),
        }
    }
}

impl core::error::Error for CosineError {}

/// Cosine similarity over dense `f32` vectors, as a model's embedding produces.
///
/// The dot product and both magnitudes accumulate in `f64` even though the
/// inputs are `f32`. Summing a few hundred `f32` products in `f32` loses
/// precision exactly where it matters — near the decision threshold, where two
/// candidates are close and the ordering is the answer. `f64::from` is exact,
/// so the wider accumulator costs nothing in fidelity and no cast.
///
/// The result is clamped to `[-1.0, 1.0]`: the arithmetic cannot leave that
/// interval, so a value outside it means the inputs were not finite, and
/// clamping keeps the metric inside the range every [`Similarity`] promises.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Cosine;

impl Cosine {
    /// Creates a cosine scorer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Calculates the cosine similarity, or reports why it is undefined.
    pub fn try_score(&self, left: &[f32], right: &[f32]) -> Result<f64, CosineError> {
        if left.len() != right.len() {
            let refusal = Err(CosineError::DimensionMismatch {
                left: left.len(),
                right: right.len(),
            });
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "try_score: returning an error to the caller");
            return refusal;
        }
        let mut dot = 0.0_f64;
        let mut left_squared = 0.0_f64;
        let mut right_squared = 0.0_f64;
        for (left_value, right_value) in left.iter().zip(right) {
            let left_value = f64::from(*left_value);
            let right_value = f64::from(*right_value);
            // Checked before it is accumulated, so a `NaN` component is named
            // as what it is rather than surfacing later as a `NaN` magnitude
            // that a caller could mistake for an all-zero vector.
            if !left_value.is_finite() || !right_value.is_finite() {
                let refusal = Err(CosineError::NonFinite);
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "try_score: returning an error to the caller");
                return refusal;
            }
            dot += left_value * right_value;
            left_squared += left_value * left_value;
            right_squared += right_value * right_value;
        }
        // Every `f32` is far below the smallest `f64` value whose square
        // overflows, so the per-component check above already implies these
        // sums are finite. The guard stays because it is the one that becomes
        // load-bearing if the element type ever widens, and because a `NaN`
        // accumulator would otherwise fall through `magnitudes <= 0.0` — `NaN`
        // compares false against everything — and return as a score.
        if !dot.is_finite() || !left_squared.is_finite() || !right_squared.is_finite() {
            let refusal = Err(CosineError::NonFinite);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "try_score: returning an error to the caller");
            return refusal;
        }
        let magnitudes = left_squared.sqrt() * right_squared.sqrt();
        if magnitudes <= 0.0 {
            let refusal = Err(CosineError::ZeroMagnitude);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "try_score: returning an error to the caller");
            return refusal;
        }
        Ok((dot / magnitudes).clamp(-1.0, 1.0))
    }

    /// Maps raw cosine similarity into the shared `[0.0, 1.0]` contract with
    /// `(raw + 1) / 2`.
    ///
    /// This is the one named mapping between the two domains. It is affine and
    /// monotone, so ordering is preserved: an opposite pair reads `0.0`, an
    /// orthogonal pair `0.5`, and an identical direction `1.0`. No threshold is
    /// shifted by it — a caller comparing against a raw-cosine threshold must
    /// call [`Self::try_score`], not this.
    ///
    /// # Errors
    ///
    /// Returns the same [`EvidenceError`] refusal variants as the vector
    /// scorers, for the same reasons.
    pub fn normalized_score(&self, left: &[f32], right: &[f32]) -> Result<f64, EvidenceError> {
        let raw = self.try_score(left, right)?;
        Ok(unit_score((raw + 1.0) / 2.0))
    }
}

impl Similarity for Cosine {
    type Value = [f32];

    /// Returns the normalized `(raw + 1) / 2` score, so an opposite pair reads
    /// `0.0` rather than escaping the trait's documented `[0.0, 1.0]` interval
    /// as `-1.0`. Use [`Cosine::try_score`] for the raw mathematical domain.
    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        lossy_score(self.normalized_score(left, right), "Cosine")
    }
}

// The compositions and the private kernels live in their own files; the
// compositions are re-exported so every path stays `similarity::<Name>`.
mod compose;
mod kernel;

pub use compose::{CheckedEvidence, CheckedSimilarity, EvidenceVerdict, Weighted, WeightedError};
use kernel::{
    geometry_score, normalize_text, normalized_path, sequence_similarity, unique_count, unit_score,
};

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
