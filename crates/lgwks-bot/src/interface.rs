//! The interface model: how a step recognizes the element it names.
//!
//! A recorded or authored step does not name an element by one selector. It
//! names it by a *recognition vector*: a set of facts observed about the
//! element, scored against the same facts observed now, with a decision made on
//! the score. This module is that model. It is pure — no I/O, no clock, no DOM —
//! because the facts arrive from a snapshot the caller already took.
//!
//! # Why the verdict is three-way
//!
//! [`Recognition`] is `Resolved` / `Ambiguous` / `Absent`, never
//! `Option<usize>`. A bare `Option` collapses two unlike situations into one
//! `None`: *nothing matched* and *several matched equally well*. The second is
//! the dangerous one. A page with two identical "Submit" buttons yields a
//! confident-looking best candidate, and a two-way verdict reports it as
//! success. The margin requirement is what makes that case unrepresentable: the
//! best candidate must lead the runner-up by a declared amount, or the result is
//! `Ambiguous` and the caller must re-ask rather than commit.
//!
//! This is the same distinction `docs/bot-on-ecs.md` §8.1 derives for a crawl
//! frontier — *a record that cannot distinguish "done" from "never started"* —
//! and the same one [`crate::error::BotError`] lacks for a timed-out `Execute`.
//! One invariant, three arrivals.
//!
//! # The document boundary is a gate, not a score
//!
//! Two elements in different frames are in different documents. Scoring across
//! that boundary is how a look-alike in an advertisement iframe outranks the
//! real control, so a candidate whose piercing path differs from the target's is
//! excluded before it is scored. Similarity is for choosing within a document;
//! identity of document is not a similarity question.

use std::fmt;

use lgwks_std::similarity::{
    EditDistance, Geometry, Jaccard, PathSimilarity, Similarity, Weighted, WeightedError,
};

/// The largest text length the default fingerprint will compare.
pub const MAX_TEXT_CHARS: usize = 512;

/// The score at or above which the default fingerprint accepts a candidate.
pub const FINGERPRINT_THRESHOLD: f64 = 0.75;

/// The lead the default fingerprint requires over the runner-up.
pub const FINGERPRINT_MARGIN: f64 = 0.1;

/// Why a recognition vector could not be constructed.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RecognitionError {
    /// The weighted composition of the vector was rejected.
    ///
    /// Carried rather than re-stated: the component rules (non-empty, weights
    /// summing to at most `1.0`, a threshold inside `[0.0, 1.0]`) belong to
    /// [`Weighted`], and duplicating them here would be a second policy that
    /// drifts from the first.
    Weights(WeightedError),
    /// The required lead over the runner-up was not a finite value in `[0.0, 1.0]`.
    InvalidMargin {
        /// The rejected margin.
        margin: f64,
    },
}

impl fmt::Display for RecognitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Weights(ref cause) => write!(formatter, "recognition vector rejected: {cause}"),
            Self::InvalidMargin { margin } => write!(
                formatter,
                "recognition margin {margin} must be finite and within [0, 1]"
            ),
        }
    }
}

impl std::error::Error for RecognitionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Weights(ref cause) => Some(cause),
            Self::InvalidMargin { .. } => None,
        }
    }
}

impl From<WeightedError> for RecognitionError {
    fn from(cause: WeightedError) -> Self {
        Self::Weights(cause)
    }
}

/// The facts observed about one element: the recognition vector's input.
///
/// Each field is a thing a snapshot reports and a fingerprint can be computed
/// over, and nothing else. Identity and mutable content are separate on purpose:
/// `tag`, `identity` and `frames` survive a redeploy, while `text` and `bounds`
/// move whenever the page's data changes. A single fused field would make every
/// data change look like a structural change — the same measured failure
/// `docs/bot-on-ecs.md` §4 records for a fused `Endpoint`.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct ElementFacts {
    /// The element's tag name, normalized to lower case.
    tag: String,
    /// Normalized `name=value` attribute pairs, with volatile values removed.
    identity: Vec<String>,
    /// The structural path, with numeric ids and generated hashes stripped.
    path: String,
    /// The element's normalized visible text.
    text: String,
    /// The normalized bounding box, as `(x, y, width, height)`.
    bounds: [f64; 4],
    /// The piercing path: frame indexes from the top document downwards.
    frames: Vec<usize>,
}

impl ElementFacts {
    /// Creates the facts for one element from the fields every snapshot carries.
    #[must_use]
    pub fn new(
        tag: impl Into<String>,
        path: impl Into<String>,
        text: impl Into<String>,
        bounds: [f64; 4],
    ) -> Self {
        Self {
            tag: tag.into(),
            identity: Vec::new(),
            path: path.into(),
            text: text.into(),
            bounds,
            frames: Vec::new(),
        }
    }

    /// Returns the same facts with the given normalized identity attributes.
    #[must_use]
    pub fn with_identity(mut self, identity: Vec<String>) -> Self {
        self.identity = identity;
        self
    }

    /// Returns the same facts with the given document-piercing path.
    #[must_use]
    pub fn with_frames(mut self, frames: Vec<usize>) -> Self {
        self.frames = frames;
        self
    }

    /// Returns the element's lower-case tag name.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Returns the normalized identity attributes.
    #[must_use]
    pub fn identity(&self) -> &[String] {
        &self.identity
    }

    /// Returns the structural path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the visible text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the normalized bounding box as `(x, y, width, height)`.
    #[must_use]
    pub const fn bounds(&self) -> [f64; 4] {
        self.bounds
    }

    /// Returns the piercing path from the top document downwards.
    #[must_use]
    pub fn frames(&self) -> &[usize] {
        &self.frames
    }
}

/// Scores two elements by their normalized identity attributes.
///
/// Identity is the strongest structural signal available: a `data-testid` or a
/// stable `id` is authored to be unique and to survive a restyle, which is
/// exactly what a locator needs and what a class path is not.
///
/// Not `Copy`, and the reason is worth recording: `Jaccard<T>` derives `Copy`
/// conservatively as `impl<T: Copy> Copy`, and `String` is not `Copy`, so the
/// `Jaccard<String>` this holds is not either. `Clone` is available and is all
/// the component needs.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct IdentityComponent {
    /// The set-similarity metric applied to the attribute collections.
    metric: Jaccard<String>,
}

impl IdentityComponent {
    /// Creates an identity component.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            metric: Jaccard::new(),
        }
    }
}

impl Default for IdentityComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Similarity for IdentityComponent {
    type Value = ElementFacts;

    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        self.metric.score(&left.identity, &right.identity)
    }
}

/// Scores two elements by their structural path, ignoring volatile segments.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct PathComponent {
    /// The structural metric applied to the two paths.
    metric: PathSimilarity,
}

impl PathComponent {
    /// Creates a structural-path component.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            metric: PathSimilarity::new(),
        }
    }
}

impl Default for PathComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Similarity for PathComponent {
    type Value = ElementFacts;

    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        self.metric.score(&left.path, &right.path)
    }
}

/// Scores two elements by their visible text.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct TextComponent {
    /// The bounded lexical metric applied to the two texts.
    metric: EditDistance,
}

impl TextComponent {
    /// Creates a text component bounded by `maximum_length` characters per side.
    #[must_use]
    pub const fn new(maximum_length: usize) -> Self {
        Self {
            metric: EditDistance::new(maximum_length),
        }
    }
}

impl Similarity for TextComponent {
    type Value = ElementFacts;

    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        self.metric.score(&left.text, &right.text)
    }
}

/// Scores two elements by where they sit in the layout.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct GeometryComponent {
    /// The geometric metric applied to the two boxes.
    metric: Geometry,
}

impl GeometryComponent {
    /// Creates a geometry component whose distance `maximum_distance` scores zero.
    #[must_use]
    pub const fn new(maximum_distance: f64) -> Self {
        Self {
            metric: Geometry::new(maximum_distance),
        }
    }
}

impl Similarity for GeometryComponent {
    type Value = ElementFacts;

    fn score(&self, left: &Self::Value, right: &Self::Value) -> f64 {
        self.metric.score(&left.bounds, &right.bounds)
    }
}

/// A weighted recognition vector, plus the lead it requires before committing.
///
/// Deliberately not `Debug`: the component list is a set of `dyn` trait objects,
/// and a derived `Debug` would print addresses rather than policy.
pub struct RecognitionVector {
    /// The composed scorer, already validated against the unit interval.
    scorer: Weighted<ElementFacts>,
    /// The minimum lead the best candidate must hold over the runner-up.
    margin: f64,
}

impl RecognitionVector {
    /// Builds a recognition vector from weighted components and its two rules.
    ///
    /// `threshold` is the score at or above which a candidate is a match at all;
    /// `margin` is the lead it must then hold over the runner-up to be the only
    /// match. Both are validated here rather than at first use, so a vector that
    /// cannot decide is refused where it is declared.
    pub fn new(
        components: Vec<(f64, Box<dyn Similarity<Value = ElementFacts>>)>,
        threshold: f64,
        margin: f64,
    ) -> Result<Self, RecognitionError> {
        if !margin.is_finite() || !(0.0..=1.0).contains(&margin) {
            return Err(RecognitionError::InvalidMargin { margin });
        }
        Ok(Self {
            scorer: Weighted::new(components, threshold)?,
            margin,
        })
    }

    /// Builds the shipped default vector for a DOM element fingerprint.
    ///
    /// The weights are `1/2` identity, `1/4` structural path, `1/8` text and
    /// `1/8` geometry. They are powers of two so they sum to exactly `1.0` in
    /// binary floating point: a perfect match scores exactly `1.0`, and the
    /// `sum(w) <= 1.0` rule [`Weighted`] enforces holds by construction rather
    /// than by rounding luck. Every other constructor below relies on that —
    /// the weight set is not a tuning knob to nudge.
    ///
    /// The ordering states an opinion rather than balancing a grid: attributes
    /// authored to be stable outrank a class path, and layout is the weakest
    /// signal because it is the one that moves when a banner loads.
    pub fn fingerprint() -> Result<Self, RecognitionError> {
        let components: Vec<(f64, Box<dyn Similarity<Value = ElementFacts>>)> = vec![
            (0.5, Box::new(IdentityComponent::new())),
            (0.25, Box::new(PathComponent::new())),
            (0.125, Box::new(TextComponent::new(MAX_TEXT_CHARS))),
            (0.125, Box::new(GeometryComponent::new(1.0))),
        ];
        Self::new(components, FINGERPRINT_THRESHOLD, FINGERPRINT_MARGIN)
    }

    /// Returns the score at or above which a candidate is a match at all.
    #[must_use]
    pub fn threshold(&self) -> f64 {
        self.scorer.threshold()
    }

    /// Returns the lead the best candidate must hold over the runner-up.
    #[must_use]
    pub const fn margin(&self) -> f64 {
        self.margin
    }

    /// Resolves which candidate the target names, as a three-way verdict.
    ///
    /// Candidates are compared in the order given and the lowest index wins a
    /// tie, so the result does not depend on iteration order — the comparator
    /// rule `docs/bot-on-ecs.md` §8 takes from Heritrix. A candidate whose
    /// piercing path differs from the target's is excluded before scoring; see
    /// the module documentation for why that is a gate and not a weight.
    #[must_use]
    pub fn recognize(&self, target: &ElementFacts, candidates: &[ElementFacts]) -> Recognition {
        let mut best: Option<(usize, f64)> = None;
        let mut next: Option<(usize, f64)> = None;

        for (index, candidate) in candidates.iter().enumerate() {
            if candidate.frames != target.frames {
                continue;
            }
            let score = self.scorer.score(target, candidate);
            match best {
                Some((_, best_score)) if score > best_score => {
                    next = best;
                    best = Some((index, score));
                }
                Some(_) => {
                    if next.is_none_or(|(_, next_score)| score > next_score) {
                        next = Some((index, score));
                    }
                }
                None => best = Some((index, score)),
            }
        }

        let Some((index, score)) = best else {
            return Recognition::Absent { best_score: 0.0 };
        };
        if score < self.scorer.threshold() {
            return Recognition::Absent { best_score: score };
        }

        let (runner_up, lead) = match next {
            Some((runner_up_index, runner_up_score)) => {
                (Some(runner_up_index), score - runner_up_score)
            }
            None => (None, score),
        };
        if lead < self.margin {
            return Recognition::Ambiguous {
                best: index,
                runner_up,
                score,
                lead,
            };
        }
        Recognition::Resolved { index, score, lead }
    }
}

/// The outcome of resolving a target element against a set of candidates.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Recognition {
    /// One candidate cleared the threshold and led by at least the margin.
    Resolved {
        /// Index of the selected candidate.
        index: usize,
        /// The selected candidate's score.
        score: f64,
        /// The lead it held over the runner-up.
        lead: f64,
    },
    /// Candidates cleared the threshold but none led by the margin.
    ///
    /// Not a failure and not a success. It is the state that must produce a
    /// re-ask, and it exists so that a caller cannot take the best of an
    /// indistinguishable field and call it a match. `runner_up` is `None` when
    /// the sole candidate's own score fell short of the margin.
    Ambiguous {
        /// Index of the highest-scoring candidate.
        best: usize,
        /// Index of the candidate it failed to separate from.
        runner_up: Option<usize>,
        /// The highest score observed.
        score: f64,
        /// The lead held over the runner-up.
        lead: f64,
    },
    /// No candidate reached the threshold.
    Absent {
        /// The highest score observed, which is below the threshold.
        best_score: f64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserts two scores agree to within the tolerance float arithmetic needs.
    ///
    /// An epsilon rather than `==`: `clippy::float_cmp` is forbidden workspace
    /// wide, and exact float equality in a test is a claim the test cannot
    /// actually check.
    fn assert_close(left: f64, right: f64) {
        assert!(
            (left - right).abs() < 1e-9,
            "expected {right}, observed {left}"
        );
    }

    /// Builds a candidate carrying one identity attribute and some visible text.
    fn candidate(identity: &str, text: &str) -> ElementFacts {
        ElementFacts::new(
            "button",
            "html > body > form > button",
            text,
            [0.5, 0.5, 0.1, 0.05],
        )
        .with_identity(vec![String::from(identity)])
    }

    /// Builds the shipped default vector.
    fn vector() -> Result<RecognitionVector, RecognitionError> {
        RecognitionVector::fingerprint()
    }

    #[test]
    fn identical_facts_resolve_with_a_full_score() -> Result<(), RecognitionError> {
        let target = candidate("data-testid=submit", "Submit");
        assert_eq!(
            vector()?.recognize(&target, std::slice::from_ref(&target)),
            Recognition::Resolved {
                index: 0,
                score: 1.0,
                lead: 1.0,
            }
        );
        Ok(())
    }

    #[test]
    fn an_empty_candidate_set_is_absent_not_a_panic() -> Result<(), RecognitionError> {
        let target = candidate("data-testid=submit", "Submit");
        assert_eq!(
            vector()?.recognize(&target, &[]),
            Recognition::Absent { best_score: 0.0 }
        );
        Ok(())
    }

    #[test]
    fn two_indistinguishable_candidates_are_ambiguous() -> Result<(), RecognitionError> {
        let target = candidate("data-testid=submit", "Submit");
        let twin = candidate("data-testid=submit", "Submit");
        assert_eq!(
            vector()?.recognize(&target, &[twin.clone(), twin]),
            Recognition::Ambiguous {
                best: 0,
                runner_up: Some(1),
                score: 1.0,
                lead: 0.0,
            },
            "the lowest index must win a tie, and the pair must not resolve"
        );
        Ok(())
    }

    #[test]
    fn a_clear_winner_resolves() -> Result<(), RecognitionError> {
        let target = candidate("data-testid=submit", "Submit order");
        let candidates = [candidate("data-testid=cancel", "Cancel"), target.clone()];
        let recognition = vector()?.recognize(&target, &candidates);
        // `lead` is the gap to the runner-up, not the score: with a competitor
        // present it is `best - next`, so this cannot be asserted as `1.0` the
        // way the no-competitor case above can. Asserting the property rather
        // than a transcribed float also keeps this test from breaking on a
        // legitimate reweight of the default vector.
        assert!(
            matches!(
                recognition,
                Recognition::Resolved { index: 1, score, lead }
                    if (score - 1.0).abs() < 1e-9 && lead >= FINGERPRINT_MARGIN
            ),
            "the second candidate must win outright with a lead over the margin, got {recognition:?}"
        );
        Ok(())
    }

    #[test]
    fn a_candidate_in_another_document_is_excluded_before_scoring() -> Result<(), RecognitionError>
    {
        let target = candidate("data-testid=submit", "Submit").with_frames(vec![0]);
        let look_alike = candidate("data-testid=submit", "Submit").with_frames(vec![3]);
        assert_eq!(
            vector()?.recognize(&target, &[look_alike]),
            Recognition::Absent { best_score: 0.0 },
            "a perfect score across a frame boundary must not be reachable"
        );
        Ok(())
    }

    #[test]
    fn a_weak_candidate_is_absent_below_the_threshold() -> Result<(), RecognitionError> {
        let target = candidate("data-testid=submit", "Submit order");
        let unrelated = ElementFacts::new(
            "section",
            "html > body > article",
            "Terms and conditions",
            [0.0, 0.0, 0.1, 0.1],
        )
        .with_identity(vec![String::from("data-testid=legal")]);
        let recognition = vector()?.recognize(&target, &[unrelated]);
        assert!(
            matches!(
                recognition,
                Recognition::Absent { best_score } if best_score < FINGERPRINT_THRESHOLD
            ),
            "an unrelated element must be Absent below the threshold, got {recognition:?}"
        );
        Ok(())
    }

    #[test]
    fn an_empty_component_set_is_refused() {
        let components: Vec<(f64, Box<dyn Similarity<Value = ElementFacts>>)> = Vec::new();
        assert_eq!(
            RecognitionVector::new(components, 0.5, 0.1).err(),
            Some(RecognitionError::Weights(WeightedError::Empty))
        );
    }

    #[test]
    fn weights_above_one_are_refused() {
        let components: Vec<(f64, Box<dyn Similarity<Value = ElementFacts>>)> = vec![
            (0.75, Box::new(IdentityComponent::new())),
            (0.75, Box::new(PathComponent::new())),
        ];
        assert_eq!(
            RecognitionVector::new(components, 0.5, 0.1).err(),
            Some(RecognitionError::Weights(
                WeightedError::WeightSumExceedsOne
            ))
        );
    }

    #[test]
    fn a_margin_outside_the_unit_interval_is_refused() {
        let components: Vec<(f64, Box<dyn Similarity<Value = ElementFacts>>)> =
            vec![(0.5, Box::new(IdentityComponent::new()))];
        assert_eq!(
            RecognitionVector::new(components, 0.5, 1.5).err(),
            Some(RecognitionError::InvalidMargin { margin: 1.5 })
        );
    }

    #[test]
    fn the_shipped_vector_carries_its_declared_rules() -> Result<(), RecognitionError> {
        let vector = vector()?;
        assert_close(vector.threshold(), FINGERPRINT_THRESHOLD);
        assert_close(vector.margin(), FINGERPRINT_MARGIN);
        assert_close(0.5 + 0.25 + 0.125 + 0.125, 1.0);
        Ok(())
    }
}
