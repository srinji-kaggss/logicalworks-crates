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
    BoundingBox, EditDistance, Geometry, Jaccard, PathSimilarity, Similarity, Weighted,
    WeightedError,
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
    /// Locator anchors this element offers, in the order the snapshot reported
    /// them. Carried rather than derived so the snapshot layer — the only part
    /// that knows how a `data-testid` was found — decides what counts as one.
    anchors: Vec<(Anchor, String)>,
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
            anchors: Vec::new(),
        }
    }

    /// Returns the same facts with the given normalized identity attributes.
    #[must_use]
    pub fn with_identity(mut self, identity: Vec<String>) -> Self {
        self.identity = identity;
        self
    }

    /// Returns the same facts with one more locator anchor.
    ///
    /// An empty value is stored as written and reads back as `None` from
    /// [`Self::anchor`], so a snapshot that could not name the element's id
    /// does not offer `Anchor::Id` rather than offering a blank one that every
    /// other blank id would match.
    #[must_use]
    pub fn with_anchor(mut self, kind: Anchor, value: impl Into<String>) -> Self {
        self.anchors.push((kind, value.into()));
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

    /// Returns the value of this element's first `kind` anchor, if it offers
    /// one with a non-empty value.
    #[must_use]
    pub fn anchor(&self, kind: Anchor) -> Option<&str> {
        self.anchors
            .iter()
            .find(|entry| entry.0 == kind && !entry.1.is_empty())
            .map(|entry| entry.1.as_str())
    }
}

/// One rung of a locator ladder: the kind of fact an anchor names.
///
/// The order is the one `docs/general-bot-fold.md` §3.2 takes from the
/// described platform's generator: a stable `id`, then a `data-testid`, then
/// the ARIA role (with its accessible name, which the snapshot layer folds into
/// the value), then visible text, then a class path. Strongest first, because
/// a class path is what survives least and is what a redeploy breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Anchor {
    /// A stable `id` attribute.
    Id,
    /// A `data-testid` or equivalent test hook.
    TestId,
    /// An ARIA role, optionally with its accessible name in the value.
    Role,
    /// The element's visible text.
    Text,
    /// A structural class or CSS path.
    ClassPath,
}

impl Anchor {
    /// Returns the rung's strength: lower is stronger.
    ///
    /// Explicit rather than a derived `Ord`, because a derived order puts the
    /// strongest variant first or last depending on declaration order, and that
    /// is a coin-flip a reader should not have to resolve.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Id => 0,
            Self::TestId => 1,
            Self::Role => 2,
            Self::Text => 3,
            Self::ClassPath => 4,
        }
    }
}

impl PartialOrd for Anchor {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Anchor {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

/// Why a [`Ladder`] could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LadderError {
    /// No anchors were given. A ladder with no rungs is not a search order.
    Empty,
}

impl fmt::Display for LadderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Empty => formatter.write_str("a locator ladder needs at least one anchor"),
        }
    }
}

impl std::error::Error for LadderError {}

/// An ordered locator ladder: anchors, strongest first, deduplicated.
///
/// The walk is `RecognitionVector::recognize_with_ladder`. This type only owns
/// the order, so a caller cannot construct a ladder whose "strongest" rung is
/// whatever happened to be declared first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ladder {
    /// Rungs, strongest first, each kind appearing once.
    anchors: Vec<Anchor>,
}

impl Ladder {
    /// Builds a ladder from any anchor list: sorted strongest-first and
    /// deduplicated, or [`LadderError::Empty`] when nothing was given.
    ///
    /// Deduplicated because two `Anchor::Text` rungs in a row are one rung and
    /// a repeated fall-through is a bug the walk should not have to notice.
    pub fn new(anchors: impl IntoIterator<Item = Anchor>) -> Result<Self, LadderError> {
        let mut sorted: Vec<Anchor> = anchors.into_iter().collect();
        if sorted.is_empty() {
            let refusal = Err(LadderError::Empty);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        sorted.sort_unstable_by_key(|anchor| anchor.rank());
        sorted.dedup();
        Ok(Self { anchors: sorted })
    }

    /// Returns the rungs, strongest first.
    #[must_use]
    pub fn anchors(&self) -> &[Anchor] {
        &self.anchors
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
        let left_bounds = BoundingBox::from(left.bounds);
        let right_bounds = BoundingBox::from(right.bounds);
        self.metric.score(&left_bounds, &right_bounds)
    }
}

/// A weighted recognition vector, plus the lead it requires before committing.
///
/// `Debug` is derived and prints the scorer through [`Weighted`]'s own manual
/// impl, which reports the composition — how many components, their weights,
/// the acceptance threshold — rather than the `dyn` trait objects behind them.
/// The margin is printed alongside it. Nothing here prints an address.
#[derive(Debug)]
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
            let refusal = Err(RecognitionError::InvalidMargin { margin });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
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
            if !is_eligible(target, candidate) {
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

    /// Resolves a target by walking `ladder` strongest-first.
    ///
    /// A rung decides by its own anchor and by nothing else. Candidates that
    /// are eligible, offer a non-empty value of that kind, and whose value
    /// equals the target's are the rung's hits; the fingerprint supplies the
    /// score fields but does not get to overrule the rung. The verdict:
    ///
    /// - **One hit** is [`Recognition::Resolved`]. A unique strong hit is the
    ///   ladder working, and its `index` is into `candidates`, not into the
    ///   rung's hit list.
    /// - **Two or more hits** is [`Recognition::Ambiguous`], returned at once
    ///   and **not** retried at a weaker rung. A weak anchor that succeeds
    ///   where a strong one was ambiguous is a likely mis-match, not a recovery:
    ///   two elements that share an `id` are not disambiguated by the fact that
    ///   only one of them happens to say "Submit" today.
    /// - **No hits** falls through. A weaker fact gets its chance. The rung
    ///   still contributes its measured `best_score` — how close the eligible
    ///   offerers came — so the final `Absent` is the strongest near-miss any
    ///   rung observed rather than a bare zero. A high-scoring candidate with
    ///   the wrong anchor remains absent: the score is evidence, not an anchor
    ///   match.
    ///
    /// A rung the target itself does not offer is skipped: there is nothing to
    /// match against, which is the same rule [`ElementFacts::anchor`] states
    /// for an empty value.
    #[must_use]
    pub fn recognize_with_ladder(
        &self,
        ladder: &Ladder,
        target: &ElementFacts,
        candidates: &[ElementFacts],
    ) -> Recognition {
        let mut strongest_absent = Recognition::Absent { best_score: 0.0 };
        for kind in ladder.anchors() {
            let Some(wanted) = target.anchor(*kind) else {
                continue;
            };
            let hits: Vec<usize> = candidates
                .iter()
                .enumerate()
                .filter(|entry| {
                    is_eligible(target, entry.1) && entry.1.anchor(*kind) == Some(wanted)
                })
                .map(|(index, _)| index)
                .collect();

            match hits.len() {
                0 => {
                    let near_miss = candidates
                        .iter()
                        .filter(|candidate| {
                            is_eligible(target, candidate) && candidate.anchor(*kind).is_some()
                        })
                        .map(|candidate| self.scorer.score(target, candidate))
                        .fold(0.0_f64, f64::max);
                    if let Recognition::Absent { best_score: prior } = strongest_absent
                        && near_miss > prior
                    {
                        strongest_absent = Recognition::Absent {
                            best_score: near_miss,
                        };
                    }
                }
                1 => {
                    let index = hits[0];
                    let score = self.scorer.score(target, &candidates[index]);
                    return Recognition::Resolved {
                        index,
                        score,
                        lead: score,
                    };
                }
                _ => {
                    let mut scored: Vec<(usize, f64)> = hits
                        .iter()
                        .map(|&index| (index, self.scorer.score(target, &candidates[index])))
                        .collect();
                    scored.sort_by(|left, right| {
                        right.1.total_cmp(&left.1).then(left.0.cmp(&right.0))
                    });
                    let (best, score) = scored[0];
                    let (runner_up, runner_up_score) = scored[1];
                    return Recognition::Ambiguous {
                        best,
                        runner_up: Some(runner_up),
                        score,
                        lead: score - runner_up_score,
                    };
                }
            }
        }
        strongest_absent
    }
}

/// Whether a candidate belongs to the target's document and kind.
///
/// This is the single eligibility gate shared by flat recognition and every
/// ladder rung. Similarity may rank eligible candidates, but it cannot grant
/// ownership across a frame boundary or between incompatible elements.
fn is_eligible(target: &ElementFacts, candidate: &ElementFacts) -> bool {
    candidate.frames == target.frames && tags_compatible(target, candidate)
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
    /// No candidate satisfied the recognizer's decision rule.
    ///
    /// Flat recognition uses this when no eligible candidate reaches the
    /// fingerprint threshold. A ladder also uses it when eligible candidates
    /// offer the wrong anchor; in that case `best_score` preserves the actual
    /// fingerprint evidence even though the anchor verdict remains absent.
    Absent {
        /// The highest fingerprint score observed for a candidate that did
        /// not satisfy the recognizer's decision rule.
        best_score: f64,
    },
}

#[cfg(test)]
mod tests;
