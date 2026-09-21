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
//! One invariant, four arrivals, and the fourth closed first: a resolver that
//! could not run at all reported a score of `0.0`, which is the same value as a
//! resolver that ran and found nothing, so `Resolution::Degraded` now carries
//! the cause instead.
//!
//! # The document boundary is a gate, not a score
//!
//! Two elements in different frames are in different documents. Scoring across
//! that boundary is how a look-alike in an advertisement iframe outranks the
//! real control, so a candidate whose piercing path differs from the target's is
//! excluded before it is scored. Similarity is for choosing within a document;
//! identity of document is not a similarity question.
//!
//! The tag is the other gate. A `button` and an `img` are not the same control
//! however alike their boxes are, and "both of these are buttons" is not a
//! degree of similarity that a weight should be able to outvote.
//!
//! # Missing evidence is not matching evidence
//!
//! A fingerprint compares two snapshots of the same element taken at different
//! moments. A fact that neither snapshot reports is not a fact the two
//! *agree* on: it is a comparison that was never made. The underlying metrics
//! cannot draw that distinction — [`Jaccard`] scores two empty sets as `1.0` by
//! the set convention every other consumer relies on, and an edit distance
//! scores two empty strings as `1.0` for the same reason — so it is drawn
//! here, at the component, where the meaning of the field is known. A fact
//! absent on either side contributes `0.0` and never more.
//!
//! Unobserved facts are not renormalized away either. Redistributing a missing
//! component's weight across the ones that remain would make a snapshot
//! carrying *less* evidence easier to match rather than harder, and geometry
//! alone could score a perfect `1.0`. The weights are fixed and the missing
//! weight stays missing, which is what lets the acceptance threshold say what
//! it means: `identity` is half the vector, so an element with no identifying
//! attribute cannot reach a threshold of three quarters however well the rest
//! of it lines up.
//!
//! [`Jaccard`]: lgwks_std::similarity::Jaccard
//! [`Recognition`]: crate::interface::Recognition

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
        // No identifying attribute on either side is not agreement about an
        // empty set of attributes. `Jaccard` reads it as `1.0`, correctly for
        // every consumer that means set equality, and here it would hand an
        // element with nothing to identify it by the *maximum* identity
        // confidence — half the fingerprint's weight, for a fact neither
        // snapshot observed. So the presence of the evidence is decided before
        // the metric is consulted: absent on either side is no confidence.
        if left.identity.is_empty() || right.identity.is_empty() {
            return 0.0;
        }
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
        // The same rule as identity, for the same reason: two empty paths are
        // not a structural agreement, they are a snapshot that reported no
        // structure. `PathSimilarity` scores the pair `1.0`.
        if left.path.is_empty() || right.path.is_empty() {
            return 0.0;
        }
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
        // An element with no visible text is ordinary — an icon-only control,
        // a spacer, an image — and two of them do not agree about their text,
        // they both have none. `EditDistance` scores the empty pair `1.0`; the
        // presence rule refuses it. An icon-only control is recognized by its
        // identity and its structure, which is what those facts are for.
        if left.text.is_empty() || right.text.is_empty() {
            return 0.0;
        }
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
        // Geometry is the one component with no absence rule, and the reason is
        // that a bounding box has no unobserved state to confuse with an
        // observed one: every snapshot reports a box for every element it
        // reports at all, and `[0.0, 0.0, 0.0, 0.0]` is the position of an
        // element at the origin rather than a fact nobody looked at. If that
        // ever stops being true the field has to become an `Option`, because a
        // sentinel value is not a presence rule.
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
    ///
    /// The weights also settle which facts a match *requires*, and that is not
    /// an accident of the numbers: a component contributes nothing when the
    /// fact is absent on either side, so a score is at most the weight of the
    /// facts both elements actually reported. Identity and path together are
    /// `3/4`, exactly the threshold, so a candidate has to carry a genuinely
    /// observed matching identifier *and* a matching structure to be accepted
    /// at all — text and layout can never make up the difference. A vector
    /// built with different weights has a different set of required facts, and
    /// `RecognitionVector::new` is where that is declared.
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
    /// rule `docs/bot-on-ecs.md` §8 takes from Heritrix. Two candidates are
    /// excluded before they are scored rather than down-weighted: one whose
    /// piercing path differs from the target's, and one whose tag is an
    /// incompatible kind of thing. See the module documentation for why those
    /// are gates and not weights.
    #[must_use]
    pub fn recognize(&self, target: &ElementFacts, candidates: &[ElementFacts]) -> Recognition {
        let mut best: Option<(usize, f64)> = None;
        let mut next: Option<(usize, f64)> = None;

        for (index, candidate) in candidates.iter().enumerate() {
            if candidate.frames != target.frames {
                continue;
            }
            if !tags_compatible(target, candidate) {
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

/// Whether two elements are the same kind of thing.
///
/// The tag is the coarsest structural fact a snapshot reports and the cheapest
/// way for two elements to be obviously different. It is a gate rather than a
/// weighted component for the same reason the frame path is: "both of these are
/// buttons" is not a degree of similarity, and a weight would let a strong
/// enough identifier score outvote a candidate that is a different HTML
/// element entirely.
///
/// An empty tag is an unreported fact rather than a claim of incompatibility,
/// so it excludes nothing. It also cannot help: the tag is not one of the
/// components, so an unknown tag leaves the candidate with exactly the evidence
/// the remaining facts supply. A *role* is not consulted here because a snapshot
/// carries it inside `identity` (`role=button`, an ARIA role attribute) and it
/// is scored there as an identifying attribute, with half the vector's weight
/// behind it.
fn tags_compatible(target: &ElementFacts, candidate: &ElementFacts) -> bool {
    target.tag.is_empty() || candidate.tag.is_empty() || target.tag == candidate.tag
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

    /// Builds facts for an element that carries a tag and a path and nothing
    /// else: no identity attributes, no text.
    fn bare(tag: &str, path: &str) -> ElementFacts {
        ElementFacts::new(tag, path, "", [0.5, 0.5, 0.1, 0.05])
    }

    /// Builds facts for an icon-only control: an identifier, a path and a box,
    /// and no visible text.
    fn icon(identifier: &str) -> ElementFacts {
        bare("button", "html > body > nav > button").with_identity(vec![String::from(identifier)])
    }

    #[test]
    fn absent_identity_is_not_evidence_that_an_unrelated_element_matches()
    -> Result<(), RecognitionError> {
        // GitHub issue #39's counterexample, verbatim. Neither fact carries an
        // identifying attribute and neither carries text; the two boxes and the
        // two paths are identical; the tags differ. This used to score
        // `Resolved { index: 0, score: 0.75, lead: 0.75 }` — identity `1.0` from
        // `Jaccard`'s empty-set convention, text `1.0` from the edit distance's,
        // geometry `1.0`, path `0.0` — a confident locator decision for an
        // element that is not even the same kind of thing.
        let target = ElementFacts::new("button", "button", "", [0.5, 0.5, 0.1, 0.05]);
        let unrelated = ElementFacts::new("img", "img", "", [0.5, 0.5, 0.1, 0.05]);
        let result = vector()?.recognize(&target, &[unrelated]);
        assert!(matches!(result, Recognition::Absent { .. }), "{result:?}");
        Ok(())
    }

    #[test]
    fn the_metric_convention_is_intact_where_it_belongs() {
        // The rule being corrected lives one layer down, and it is still
        // correct there: two empty sets are the same set and two empty strings
        // are the same string. What element recognition changes is the reading
        // of "empty" — *nothing to compare* rather than *identical* — and this
        // asserts both halves, so a future repair cannot quietly change the
        // shared metric to fix a consumer.
        let no_attributes: [String; 0] = [];
        assert_close(
            Jaccard::<String>::new().score(&no_attributes, &no_attributes),
            1.0,
        );
        assert_close(EditDistance::new(8).score("", ""), 1.0);
        assert_close(PathSimilarity::new().score("", ""), 1.0);

        let blank = bare("button", "");
        assert_close(IdentityComponent::new().score(&blank, &blank), 0.0);
        assert_close(TextComponent::new(8).score(&blank, &blank), 0.0);
        assert_close(PathComponent::new().score(&blank, &blank), 0.0);
    }

    #[test]
    fn an_element_with_nothing_to_identify_it_by_cannot_resolve() -> Result<(), RecognitionError> {
        // Same tag, same path, same box, no text, and no identity attributes on
        // either side. Every fact that is present agrees perfectly, and the two
        // snapshots still say nothing about which element this is: the absent
        // facts are comparisons that were not made, not comparisons that
        // succeeded. Identity is half the vector and the threshold is three
        // quarters, so it cannot resolve however well the rest lines up.
        let recognition = vector()?.recognize(
            &bare("button", "html > body > form > button"),
            &[bare("button", "html > body > form > button")],
        );
        assert!(
            matches!(
                recognition,
                Recognition::Absent { best_score } if best_score < FINGERPRINT_THRESHOLD
            ),
            "two elements with no identifying evidence must not match, got {recognition:?}"
        );
        Ok(())
    }

    #[test]
    fn an_icon_only_control_resolves_on_its_identifier() -> Result<(), RecognitionError> {
        // The positive case the presence rule must not break: no visible text
        // at all, and a genuinely observed matching stable identifier. Text is
        // absent on both sides and contributes nothing; identity and structure
        // carry the decision, which is what those facts are for.
        let target = icon("data-testid=menu");
        let recognition = vector()?.recognize(&target, std::slice::from_ref(&target));
        assert!(
            matches!(
                recognition,
                Recognition::Resolved { index: 0, score, lead }
                    if (score - 0.875).abs() < 1e-9 && lead >= FINGERPRINT_MARGIN
            ),
            "a matching identifier must still resolve an icon-only control, got {recognition:?}"
        );
        Ok(())
    }

    #[test]
    fn an_icon_only_control_does_not_match_on_layout() -> Result<(), RecognitionError> {
        // The same icon-only pair with different identifiers. Identity is
        // unobserved-in-effect (both sides report one, and they disagree) and
        // text is absent, so the only facts left are the path and the box, worth
        // a quarter of the vector between them — nowhere near three quarters.
        // Layout alone cannot locate a control.
        let target = icon("data-testid=menu");
        let other = icon("data-testid=close");
        let recognition = vector()?.recognize(&target, &[other]);
        assert!(
            matches!(
                recognition,
                Recognition::Absent { best_score } if best_score < FINGERPRINT_THRESHOLD
            ),
            "a quarter of the vector must not resolve, got {recognition:?}"
        );
        Ok(())
    }

    #[test]
    fn an_unrelated_tag_is_excluded_however_well_the_rest_agrees() -> Result<(), RecognitionError> {
        // Every component in the vector agrees perfectly, including a genuinely
        // observed matching identifier and matching text. The tag disagrees, and
        // the tag is a gate, so the candidate is never scored. The mirror case
        // below is what shows the gate is the cause rather than the scoring.
        let facts = |tag: &str| {
            ElementFacts::new(
                tag,
                "html > body > form > button",
                "Submit",
                [0.5, 0.5, 0.1, 0.05],
            )
            .with_identity(vec![String::from("data-testid=submit")])
        };
        let target = facts("button");
        let impostor = facts("img");
        assert_eq!(
            vector()?.recognize(&target, &[impostor]),
            Recognition::Absent { best_score: 0.0 },
            "a different element kind is not a candidate"
        );
        let twin = facts("button");
        assert!(
            matches!(
                vector()?.recognize(&target, &[twin]),
                Recognition::Resolved { score, .. } if (score - 1.0).abs() < 1e-9
            ),
            "the identical element on the same facts still resolves"
        );
        Ok(())
    }

    #[test]
    fn an_unreported_tag_excludes_nothing() -> Result<(), RecognitionError> {
        // An empty tag is a fact nobody reported, not a competing one, so it
        // cannot be an incompatibility. It cannot help either: the tag is not a
        // component, so the candidate has exactly the evidence its other facts
        // supply, and it still has to clear the threshold on those.
        let target = ElementFacts::new(
            "button",
            "html > body > form > button",
            "Submit",
            [0.5, 0.5, 0.1, 0.05],
        )
        .with_identity(vec![String::from("data-testid=submit")]);
        let unlabelled = ElementFacts::new(
            "",
            "html > body > form > button",
            "Submit",
            [0.5, 0.5, 0.1, 0.05],
        )
        .with_identity(vec![String::from("data-testid=submit")]);
        let recognition = vector()?.recognize(&target, &[unlabelled]);
        assert!(
            matches!(recognition, Recognition::Resolved { index: 0, .. }),
            "an unreported tag must not exclude an otherwise matching element, got {recognition:?}"
        );
        Ok(())
    }

    #[test]
    fn structurally_distinct_candidates_are_measured_and_rejected() -> Result<(), RecognitionError>
    {
        // Same tag, so the candidate is scored rather than gated, and every
        // fact it reports differs. The score is a real measurement below the
        // threshold, which is the difference between "looked at and rejected"
        // and "never looked at".
        let target = ElementFacts::new(
            "button",
            "html > body > form > button",
            "Submit order",
            [0.5, 0.5, 0.1, 0.05],
        )
        .with_identity(vec![String::from("data-testid=submit")]);
        let elsewhere = ElementFacts::new(
            "button",
            "html > body > article > aside > button",
            "Terms and conditions",
            [900.0, 900.0, 0.1, 0.05],
        )
        .with_identity(vec![String::from("data-testid=legal")]);
        let recognition = vector()?.recognize(&target, &[elsewhere]);
        assert!(
            matches!(
                recognition,
                Recognition::Absent { best_score }
                    if best_score < FINGERPRINT_THRESHOLD && best_score > 0.0
            ),
            "a scored near-miss is Absent at its measured score, got {recognition:?}"
        );
        Ok(())
    }

    #[test]
    fn two_indistinguishable_icon_only_candidates_are_ambiguous() -> Result<(), RecognitionError> {
        // The same absence rules, with a field instead of a lone candidate: two
        // identical icon-only controls carry the same observed identifier, so
        // neither can be separated from the other, and the verdict is the one
        // that forces a re-ask rather than a guess.
        let target = icon("data-testid=menu");
        let recognition = vector()?.recognize(&target, &[target.clone(), target.clone()]);
        assert!(
            matches!(
                &recognition,
                Recognition::Ambiguous { best: 0, runner_up: Some(1), score, lead }
                    if (score - 0.875).abs() < 1e-9 && lead.abs() < 1e-9
            ),
            "two indistinguishable icon-only controls are a tie, got {recognition:?}"
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
