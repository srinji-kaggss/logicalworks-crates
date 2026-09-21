//! The semantic tier: resolving an utterance a lexicon cannot reach.
//!
//! [`crate::language`] matches letters. It handles spelling, sound, and a
//! learned alias table, and it is deliberately blind to meaning: `"the usual"`
//! and `"Repeat last order"` share no token, no sound-alike key, and no useful
//! edit distance, so no amount of string comparison relates them. That is the
//! documented limit of a lexicon, and this module is the layer that answers it —
//! a comparison in a model's embedding space, where two phrases that mean the
//! same thing are near each other regardless of the words used.
//!
//! # The deterministic tiers stay authoritative
//!
//! This tier is consulted only when the lexicon returns [`Resolution::Absent`] —
//! when every option was considered and none fit. A lexicon verdict, including
//! [`Resolution::Ambiguous`], is returned verbatim and never rescored. Two
//! reasons, and the second is the load-bearing one:
//!
//! - A deterministic verdict is reproducible from the input alone, and the
//!   cheapest way to keep that property is not to ask a model when the answer is
//!   already known.
//! - A model is a *tie-breaker of last resort*, not an override. If it could
//!   outvote an exact match it would eventually do so, and an exact match is the
//!   one verdict that cannot be wrong.
//!
//! The consequence is that this module can only ever turn an [`Resolution::Absent`]
//! into something else. It cannot change a verdict the lexicon already reached,
//! so adding it is not a behaviour change for any utterance that already
//! resolved — which is what makes it safe to introduce into a running system.
//!
//! # The embedder is injected, and its identity is recorded
//!
//! [`Embedder`] is a seam, not a dependency. This crate carries no model, no
//! tensor library and no tokenizer: the weights are not source, and a crate that
//! compiled one in would make every consumer pay for it. A consumer that has a
//! model supplies it.
//!
//! What the seam *does* require is [`Embedder::identity`], because a verdict a
//! model produced is only declarable if the model is named. "The semantic tier
//! resolved this" is not an audit record; "`all-MiniLM-L6-v2`, digest `ab12…`,
//! 384 dimensions, resolved this" is. The digest is supplied by the embedder
//! rather than computed here because only the code that loaded the weights can
//! know them.
//!
//! # Failure is a verdict, and the record is the journal
//!
//! An embedder can fail, and a failed embedder must not be reported as *nothing
//! matched*. The two need different repairs — rephrase, versus restore the
//! dependency — and they would otherwise render identically. This tier therefore
//! returns [`Resolution::Degraded`], and a caller that does not distinguish it
//! still re-asks, which is the safe direction.
//!
//! This module does not log. It reports. The cause travels to the caller in the
//! verdict and is written into the session transcript, which is the run's
//! declared record; a log line would be a second, unmanaged copy of the same
//! fact, and this crate has no logging edge to write one through. An
//! [`Embedder`] that fails is expected to describe its own failure — it owns its
//! error type and the context around it — while the resolver's job is the closed
//! verdict the flow can route on.
//!
//! # What this module does not do
//!
//! It does not cache. Every decision embeds the utterance and each option, so a
//! session that asks the same question twice embeds the options twice. That is
//! deliberate for a first landing: the option list is authored and short, a
//! local model embeds a short phrase in microseconds, and a cache is a second
//! piece of state that needs a bound, an eviction policy, and a test for both.
//! The seam it would go behind is [`Embedder`], so adding one is a change here
//! and not a change to any caller.

use std::fmt;

use lgwks_std::similarity::{Cosine, CosineError};

use crate::language::{LanguageResolver, decide};
use crate::session::{DegradedReason, MatchTier, Question, Resolution, Resolver};

/// A source of dense vector representations for text, and its own identity.
///
/// The identity is required rather than optional because a resolution this tier
/// produces is recorded as having come from a model, and a record that names no
/// model cannot be reproduced.
///
/// Both the identity and the vector length are fixed for an embedder's lifetime:
/// a provider that returned 384 dimensions for one call and 768 for the next
/// would make every score a comparison of unlike things. This tier checks each
/// returned vector against [`EmbedderIdentity::dimension`] and reports the
/// mismatch rather than scoring it.
pub trait Embedder {
    /// The error a failed embedding reports.
    ///
    /// The resolver maps any error to [`DegradedReason::EmbedderUnavailable`]
    /// and does not carry the error forward: the verdict has to be a closed set
    /// the flow can route on, and a provider's error type is open. An
    /// implementation is therefore responsible for recording its own failure
    /// detail where it can be found.
    type Error: std::error::Error + 'static;

    /// Returns the identity recorded with every decision this embedder informs.
    fn identity(&self) -> &EmbedderIdentity;

    /// Embeds one text.
    fn embed(&self, text: &str) -> Result<Vec<f32>, Self::Error>;

    /// Embeds a batch, preserving input order.
    ///
    /// The default calls [`Self::embed`] per item, which is correct and is what
    /// a provider without a real batched path should do. Override it when the
    /// provider can embed a batch in one pass, which is the usual case for a
    /// transformer: batching is the difference between `n` forward passes and
    /// one, and the default cannot know that.
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Self::Error> {
        texts.iter().map(|text| self.embed(text)).collect()
    }
}

/// What a decision made by a model is recorded as having come from.
///
/// Three fields, each answering a different question. `name` is what a person
/// calls the model. `digest` is what makes two runs comparable: two models with
/// the same name and different weights are different models, and only the digest
/// tells them apart. `dimension` is what makes the scores comparable to each
/// other, and a comparison across dimensions is meaningless rather than merely
/// inaccurate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EmbedderIdentity {
    /// The model's name, as a person would say it.
    name: String,
    /// A content digest of the weights, as the loader computed it.
    digest: String,
    /// The length of every vector the embedder returns.
    dimension: usize,
}

impl EmbedderIdentity {
    /// Names one embedder, refusing the states that make a score meaningless.
    ///
    /// Each refusal removes a way for the record to be useless. An empty name
    /// records nothing; an empty digest makes two different weight sets
    /// indistinguishable, which defeats the field's only purpose; a zero
    /// dimension means no vector could ever match, so every score would be a
    /// mismatch and the tier could only ever report degraded.
    pub fn new(
        name: impl Into<String>,
        digest: impl Into<String>,
        dimension: usize,
    ) -> Result<Self, SemanticError> {
        let name = name.into();
        let digest = digest.into();
        if name.is_empty() {
            return Err(SemanticError::UnnamedEmbedder);
        }
        if digest.is_empty() {
            return Err(SemanticError::UndigestedEmbedder { name });
        }
        if dimension == 0 {
            return Err(SemanticError::ZeroDimension { name });
        }
        Ok(Self {
            name,
            digest,
            dimension,
        })
    }

    /// Returns the model's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the content digest of the weights.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns the length of every vector the embedder returns.
    #[must_use]
    pub const fn dimension(&self) -> usize {
        self.dimension
    }
}

impl fmt::Display for EmbedderIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} (digest {}, {} dimensions)",
            self.name, self.digest, self.dimension
        )
    }
}

/// Why the semantic tier could not be constructed.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SemanticError {
    /// The embedder was named with an empty name.
    UnnamedEmbedder,
    /// The embedder was given no weight digest, so a run could not be reproduced.
    UndigestedEmbedder {
        /// The model's name, for the caller's diagnostic.
        name: String,
    },
    /// The embedder declared a zero-width vector.
    ZeroDimension {
        /// The model's name, for the caller's diagnostic.
        name: String,
    },
    /// The acceptance threshold was not a finite number.
    ///
    /// Separate from [`Self::InvalidThreshold`] because it cannot carry the
    /// rejected value. A threshold of `f64::NAN` is not equal to itself, so an
    /// error that carried it would not be equal to an identically constructed
    /// copy of itself — `PartialEq` stops being reflexive, and `assert_eq!` on
    /// such an error can never pass. "Not a number" and "outside the interval"
    /// are also different mistakes with different repairs, so splitting them
    /// costs nothing and removes the trap.
    NonFiniteThreshold,
    /// The acceptance threshold was finite but outside `[0.0, 1.0]`.
    InvalidThreshold {
        /// The rejected threshold.
        threshold: f64,
    },
    /// The required lead was not a finite number.
    NonFiniteMargin,
    /// The required lead was finite but outside `[0.0, 1.0]`.
    InvalidMargin {
        /// The rejected margin.
        margin: f64,
    },
}

impl fmt::Display for SemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::UnnamedEmbedder => {
                formatter.write_str("a semantic embedder must be named to be recorded")
            }
            Self::UndigestedEmbedder { ref name } => write!(
                formatter,
                "semantic embedder {name} has no weight digest, so a run using it cannot be reproduced"
            ),
            Self::ZeroDimension { ref name } => write!(
                formatter,
                "semantic embedder {name} declares a zero-width vector"
            ),
            Self::InvalidThreshold { threshold } => write!(
                formatter,
                "semantic threshold {threshold} must be within [0, 1]"
            ),
            Self::NonFiniteThreshold => {
                formatter.write_str("the semantic threshold must be a finite number")
            }
            Self::NonFiniteMargin => {
                formatter.write_str("the semantic margin must be a finite number")
            }
            Self::InvalidMargin { margin } => {
                write!(formatter, "semantic margin {margin} must be within [0, 1]")
            }
        }
    }
}

impl std::error::Error for SemanticError {}

/// The score a semantic decision accepts, and the lead it demands.
///
/// Two values rather than one, because they answer different questions. The
/// threshold asks *is this option close enough to be the answer?* and the margin
/// asks *is it clearly closer than the next one?* An embedder that puts every
/// option at `0.9` is an embedder that distinguishes nothing, and only the
/// margin rejects it.
///
/// Separate from [`crate::language`]'s constants and not shared with them: a
/// cosine between two short phrases and a blended lexical score are different
/// quantities on different scales, and one threshold over both would be a
/// category error that happened to compile. The defaults are **declared policy,
/// not fitted values** — a starting point a caller is expected to calibrate
/// against its own model, which is why they are constructor arguments rather
/// than constants the tier reads directly.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct SemanticPolicy {
    /// The cosine at or above which an option is a candidate.
    threshold: f64,
    /// The lead the best candidate must hold over the runner-up.
    margin: f64,
}

impl SemanticPolicy {
    /// The threshold and margin this tier ships with.
    ///
    /// `0.72` sits above the cosine that unrelated short phrases typically reach
    /// under a sentence-embedding model and below the cosine a paraphrase
    /// reaches, which is the band that makes the tier worth having. The `0.05`
    /// margin is deliberately small: two phrasings of one option legitimately
    /// score within a few hundredths of each other, and a large margin would
    /// report that as ambiguous rather than choosing.
    pub const DEFAULT: Self = Self {
        threshold: 0.72,
        margin: 0.05,
    };

    /// Validates a threshold and margin.
    pub fn new(threshold: f64, margin: f64) -> Result<Self, SemanticError> {
        if !threshold.is_finite() {
            return Err(SemanticError::NonFiniteThreshold);
        }
        if !(0.0..=1.0).contains(&threshold) {
            return Err(SemanticError::InvalidThreshold { threshold });
        }
        if !margin.is_finite() {
            return Err(SemanticError::NonFiniteMargin);
        }
        if !(0.0..=1.0).contains(&margin) {
            return Err(SemanticError::InvalidMargin { margin });
        }
        Ok(Self { threshold, margin })
    }

    /// Returns the acceptance threshold.
    #[must_use]
    pub const fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Returns the required lead over the runner-up.
    #[must_use]
    pub const fn margin(&self) -> f64 {
        self.margin
    }
}

impl Default for SemanticPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The lexicon, with an embedding comparison behind it.
///
/// A [`Resolver`] that is a strict superset of [`LanguageResolver`] by
/// construction: it *contains* one and returns its verdict verbatim whenever
/// that verdict is anything but [`Resolution::Absent`]. It is not a second
/// implementation of the same job — it cannot disagree with the lexicon, because
/// the lexicon's answer is the answer. What it adds is the case the lexicon
/// answers `Absent`.
///
/// Deliberately not `Debug`, for the reason [`LanguageResolver`] is not: the
/// learned alias table is the interesting state, and a derived `Debug` would
/// print it in whatever order the map iterates.
#[non_exhaustive]
pub struct SemanticResolver<E> {
    /// The deterministic tiers, consulted first and authoritatively.
    lexicon: LanguageResolver,
    /// The injected model.
    embedder: E,
    /// The acceptance policy for the semantic tier.
    policy: SemanticPolicy,
}

impl<E: Embedder> SemanticResolver<E> {
    /// Wraps an embedder with the shipped policy.
    #[must_use]
    pub fn new(embedder: E) -> Self {
        Self {
            lexicon: LanguageResolver::new(),
            embedder,
            policy: SemanticPolicy::DEFAULT,
        }
    }

    /// Wraps an embedder with a policy that has already been validated.
    #[must_use]
    pub fn with_policy(embedder: E, policy: SemanticPolicy) -> Self {
        Self {
            lexicon: LanguageResolver::new(),
            embedder,
            policy,
        }
    }

    /// Returns the identity recorded with every decision this resolver informs.
    #[must_use]
    pub fn embedder_identity(&self) -> &EmbedderIdentity {
        self.embedder.identity()
    }

    /// Returns the acceptance policy in force.
    #[must_use]
    pub const fn policy(&self) -> SemanticPolicy {
        self.policy
    }

    /// Records a confirmed alias on the deterministic tier.
    ///
    /// Delegated rather than reimplemented: an alias is a lexicon fact, and a
    /// person who confirms a resolution has taught the deterministic tier
    /// something it will answer from without a model. That is the direction of
    /// travel this module wants — a confirmed correction should move a phrase
    /// *out* of the semantic tier and into the reproducible one.
    pub fn learn(&mut self, utterance: &str, index: usize) -> Option<usize> {
        self.lexicon.learn(utterance, index)
    }

    /// Removes a learned alias, returning whether one was present.
    pub fn forget(&mut self, utterance: &str) -> bool {
        self.lexicon.forget(utterance)
    }

    /// Returns the number of learned aliases.
    #[must_use]
    pub fn learned(&self) -> usize {
        self.lexicon.learned()
    }

    /// Embeds one text, mapping every provider failure to a degraded reason.
    fn embed(&self, text: &str) -> Result<Vec<f32>, DegradedReason> {
        match self.embedder.embed(text) {
            Ok(vector) if vector.len() == self.embedder.identity().dimension() => Ok(vector),
            // A vector of the wrong length is not a value to score. Scoring it
            // is either a panic or a silent comparison of unlike things, and
            // reporting "nothing matched" would blame the utterance for a
            // provider defect.
            Ok(_) | Err(_) => Err(DegradedReason::EmbedderUnavailable),
        }
    }

    /// Scores every option against the utterance, best-first.
    ///
    /// An option below the threshold is excluded, and so is one whose vector has
    /// zero magnitude: its angle to anything is undefined, which is the absence
    /// of a score rather than a score of zero.
    fn score_semantically(
        &self,
        utterance: &str,
        options: &[String],
    ) -> Result<Vec<(usize, MatchTier, f64)>, DegradedReason> {
        let target = self.embed(utterance)?;
        let metric = Cosine::new();

        let mut scored: Vec<(usize, MatchTier, f64)> = Vec::with_capacity(options.len());
        for (index, option) in options.iter().enumerate() {
            let candidate = self.embed(option)?;
            match metric.try_score(&target, &candidate) {
                Ok(score) => {
                    if score >= self.policy.threshold() {
                        scored.push((index, MatchTier::Semantic, score));
                    }
                }
                // The two refusals are not the same refusal. A dimension
                // mismatch means the provider contradicted its own declared
                // identity, which no candidate can recover from. A degenerate
                // vector is one option the tier cannot speak about, and the
                // remaining options are still comparable.
                Err(CosineError::DimensionMismatch { .. }) => {
                    return Err(DegradedReason::EmbedderUnavailable);
                }
                Err(CosineError::ZeroMagnitude) => {}
                // `CosineError` is `#[non_exhaustive]`, so a future refusal
                // reaches here. It defaults to the conservative reading — this
                // tier cannot speak about this one candidate — because the
                // alternative, degrading the whole decision, would let an
                // unrelated future variant stop a resolution that is otherwise
                // available. A refusal that warrants degrading is a refusal
                // about the provider, and `DimensionMismatch` above is that
                // case; a new refusal about a *value* belongs here.
                Err(_) => {}
            }
        }

        // Stable sort by descending score, matching the lexicon: the input is in
        // ascending index order, so equal scores keep lowest-index-first.
        scored.sort_by(|left, right| {
            right
                .2
                .partial_cmp(&left.2)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(scored)
    }
}

impl<E: Embedder> Resolver for SemanticResolver<E> {
    fn resolve(&self, utterance: &str, question: &Question<'_>) -> Resolution {
        let deterministic = self.lexicon.decide_for(utterance, question);
        if !matches!(deterministic, Resolution::Absent { .. }) {
            return deterministic;
        }
        // The lexicon considered every option and none fit. This is the only
        // path on which a model is consulted, so no verdict the lexicon reached
        // can be changed by enabling this tier.
        match self.score_semantically(utterance, question.options()) {
            Ok(scored) => decide(&scored, self.policy.margin()),
            Err(reason) => Resolution::Degraded { reason },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Embedder, EmbedderIdentity, SemanticError, SemanticPolicy, SemanticResolver};
    use crate::session::{DegradedReason, MatchTier, Question, Resolution, Resolver};
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::fmt;
    use std::rc::Rc;

    /// The failure a stub embedder reports, to exercise the degraded path.
    #[derive(Debug)]
    struct StubFailure;

    impl fmt::Display for StubFailure {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("stub embedder failure")
        }
    }

    impl std::error::Error for StubFailure {}

    /// An embedder whose vectors the test chooses, counting its own calls.
    ///
    /// The counter is shared through an `Rc` rather than exposed by an accessor
    /// on the resolver: the point of several of these tests is that the model is
    /// *not* consulted, and a public accessor added only to observe that would
    /// be API surface justified by a test.
    struct StubEmbedder {
        identity: EmbedderIdentity,
        vectors: BTreeMap<String, Vec<f32>>,
        fallback: Vec<f32>,
        calls: Rc<Cell<usize>>,
        failing: bool,
    }

    impl Embedder for StubEmbedder {
        type Error = StubFailure;

        fn identity(&self) -> &EmbedderIdentity {
            &self.identity
        }

        fn embed(&self, text: &str) -> Result<Vec<f32>, Self::Error> {
            self.calls.set(self.calls.get().saturating_add(1));
            if self.failing {
                return Err(StubFailure);
            }
            Ok(self
                .vectors
                .get(text)
                .cloned()
                .unwrap_or_else(|| self.fallback.clone()))
        }
    }

    const DIMENSION: usize = 2;

    /// Builds a stub embedder and the call counter the test watches.
    ///
    /// `fallback` is returned for any text not in `vectors`, so a test can put a
    /// deliberately wrong-length vector there and reach the mismatch path.
    fn stub(
        vectors: Vec<(&str, Vec<f32>)>,
        fallback: Vec<f32>,
        failing: bool,
    ) -> Result<(StubEmbedder, Rc<Cell<usize>>), SemanticError> {
        let calls = Rc::new(Cell::new(0));
        let embedder = StubEmbedder {
            identity: EmbedderIdentity::new("stub-model", "digest-0123456789", DIMENSION)?,
            vectors: vectors
                .into_iter()
                .map(|(text, vector)| (text.to_owned(), vector))
                .collect(),
            fallback,
            calls: Rc::clone(&calls),
            failing,
        };
        Ok((embedder, calls))
    }

    fn options(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// Names the question the resolver tests resolve against.
    fn ask(options: &[String]) -> Question<'_> {
        Question::new("ask", options)
    }

    /// The two vectors a paraphrase relationship is built from, plus an
    /// unrelated one. `"the usual"` and `"Repeat last order"` share no word and
    /// no sound-alike key, which is the point: the lexicon cannot relate them,
    /// and the embedder can.
    fn paraphrase_vectors() -> Vec<(&'static str, Vec<f32>)> {
        vec![
            ("the usual", vec![1.0, 0.0]),
            ("Repeat last order", vec![1.0, 0.1]),
            ("Cancel", vec![0.0, 1.0]),
            ("Later", vec![-1.0, 0.0]),
        ]
    }

    /// Asserts two vectors match component-wise, without comparing floats by
    /// equality: `float_cmp` is forbidden workspace-wide, and a paraphrase
    /// assertion is about proximity rather than identity.
    fn assert_vector(actual: &[f32], expected: &[f32]) {
        assert_eq!(
            actual.len(),
            expected.len(),
            "vector length changed: {actual:?} vs {expected:?}"
        );
        for (index, (actual_component, expected_component)) in
            actual.iter().zip(expected).enumerate()
        {
            assert!(
                (actual_component - expected_component).abs() < 1e-6,
                "component {index} was {actual_component}, expected {expected_component}"
            );
        }
    }

    #[test]
    fn an_exact_lexicon_match_never_consults_the_model() -> Result<(), Box<dyn std::error::Error>> {
        let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
        let resolver = SemanticResolver::new(embedder);

        let resolution = resolver.resolve("yes", &ask(&options(&["yes", "no"])));

        assert_eq!(
            resolution,
            Resolution::Resolved {
                index: 0,
                tier: MatchTier::Exact,
                score: 1.0,
                lead: 1.0,
            },
            "the lexicon's verdict is returned unchanged"
        );
        assert_eq!(
            calls.get(),
            0,
            "an exact match must not reach the embedder at all"
        );
        Ok(())
    }

    #[test]
    fn a_lexicon_tie_is_not_rescored_by_the_model() -> Result<(), Box<dyn std::error::Error>> {
        // `Jaccard` scores both options identically against a shared word, so the
        // lexicon reports a tie. The model is far more decisive, and its opinion
        // is exactly what must not be heard here: a tie is a question to put back
        // to the person, not a case for a model to settle.
        let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
        let resolver = SemanticResolver::new(embedder);

        let resolution = resolver.resolve("order", &ask(&options(&["order", "order"])));

        assert!(
            matches!(resolution, Resolution::Ambiguous { .. }),
            "the lexicon's ambiguity survives, got {resolution:?}"
        );
        assert_eq!(
            calls.get(),
            0,
            "an ambiguous verdict must not reach the model"
        );
        Ok(())
    }

    #[test]
    fn a_phrase_the_lexicon_cannot_reach_resolves_semantically()
    -> Result<(), Box<dyn std::error::Error>> {
        let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
        let resolver = SemanticResolver::new(embedder);
        let options = options(&["Repeat last order", "Cancel"]);

        // Establish the premise: the lexicon alone cannot reach this phrase.
        let lexicon = crate::language::LanguageResolver::new();
        assert!(
            matches!(
                lexicon.resolve("the usual", &ask(&options)),
                Resolution::Absent { .. }
            ),
            "\"the usual\" must not be reachable by letters alone, or this test proves nothing"
        );

        let resolution = resolver.resolve("the usual", &ask(&options));

        assert!(
            matches!(
                resolution,
                Resolution::Resolved {
                    index: 0,
                    tier: MatchTier::Semantic,
                    ..
                }
            ),
            "expected a semantic resolution of option 0, got {resolution:?}"
        );
        assert!(
            calls.get() > 0,
            "the model is what resolved it, so it must have been consulted"
        );
        Ok(())
    }

    #[test]
    fn two_equally_close_options_are_ambiguous_rather_than_guessed()
    -> Result<(), Box<dyn std::error::Error>> {
        let (embedder, _calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
        let resolver = SemanticResolver::new(embedder);

        // Both options are the same vector, so no margin can separate them. The
        // tier must report the tie rather than let index order decide, which is
        // the whole reason the verdict is three-way.
        let resolution = resolver.resolve(
            "the usual",
            &ask(&options(&["Repeat last order", "Repeat last order"])),
        );

        assert!(
            matches!(&resolution, Resolution::Ambiguous { tied, .. } if *tied == vec![0, 1]),
            "both options are still in play, got {resolution:?}"
        );
        Ok(())
    }

    #[test]
    fn an_option_below_the_threshold_is_not_a_candidate() -> Result<(), Box<dyn std::error::Error>>
    {
        let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
        let resolver = SemanticResolver::new(embedder);

        // Both options are orthogonal or opposed to the utterance, so the tier
        // has nothing to say and must say nothing rather than pick the least bad.
        let resolution = resolver.resolve("the usual", &ask(&options(&["Cancel", "Later"])));

        assert!(
            matches!(resolution, Resolution::Absent { .. }),
            "expected the semantic tier to stay silent, got {resolution:?}"
        );
        assert!(calls.get() > 0, "the tier did look, and found nothing");
        Ok(())
    }

    #[test]
    fn a_failing_embedder_is_degraded_rather_than_absent() -> Result<(), Box<dyn std::error::Error>>
    {
        let (embedder, _calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], true)?;
        let resolver = SemanticResolver::new(embedder);

        let resolution = resolver.resolve(
            "the usual",
            &ask(&options(&["Repeat last order", "Cancel"])),
        );

        assert_eq!(
            resolution,
            Resolution::Degraded {
                reason: DegradedReason::EmbedderUnavailable,
            },
            "a failed embedder is not a person who said something unclear"
        );
        Ok(())
    }

    #[test]
    fn a_vector_of_the_wrong_length_is_degraded_rather_than_scored()
    -> Result<(), Box<dyn std::error::Error>> {
        // The fallback is three wide against a declared dimension of two, so any
        // option not named in `vectors` comes back the wrong size. Scoring it
        // would compare unlike things; this asserts it is reported instead.
        let (embedder, _calls) = stub(
            vec![("the usual", vec![1.0, 0.0])],
            vec![0.0, 1.0, 2.0],
            false,
        )?;
        let resolver = SemanticResolver::new(embedder);

        let resolution = resolver.resolve(
            "the usual",
            &ask(&options(&["Repeat last order", "Cancel"])),
        );

        assert_eq!(
            resolution,
            Resolution::Degraded {
                reason: DegradedReason::EmbedderUnavailable,
            },
            "a provider that contradicts its own declared width is broken, not silent"
        );
        Ok(())
    }

    #[test]
    fn a_learned_alias_moves_a_phrase_off_the_model() -> Result<(), Box<dyn std::error::Error>> {
        let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
        let mut resolver = SemanticResolver::new(embedder);
        let options = options(&["Repeat last order", "Cancel"]);

        assert_eq!(
            resolver.learn("the usual", 0),
            None,
            "the first binding for a phrase has no predecessor"
        );

        let resolution = resolver.resolve("the usual", &ask(&options));
        assert_eq!(
            resolution,
            Resolution::Resolved {
                index: 0,
                tier: MatchTier::Exact,
                score: 1.0,
                lead: 1.0,
            },
            "a confirmed phrase resolves exactly from then on"
        );
        assert_eq!(
            calls.get(),
            0,
            "and a confirmed correction leaves the model out of it entirely"
        );
        assert_eq!(resolver.learned(), 1);
        assert!(resolver.forget("the usual"));
        assert_eq!(resolver.learned(), 0);
        Ok(())
    }

    #[test]
    fn identity_refuses_a_record_that_could_not_be_reproduced() {
        assert_eq!(
            EmbedderIdentity::new("", "digest", 4),
            Err(SemanticError::UnnamedEmbedder)
        );
        assert_eq!(
            EmbedderIdentity::new("model", "", 4),
            Err(SemanticError::UndigestedEmbedder {
                name: String::from("model")
            })
        );
        assert_eq!(
            EmbedderIdentity::new("model", "digest", 0),
            Err(SemanticError::ZeroDimension {
                name: String::from("model")
            })
        );
        let identity = EmbedderIdentity::new("model", "digest", 4);
        assert!(
            identity.is_ok(),
            "a named, digested, sized model is accepted"
        );
    }

    #[test]
    fn policy_refuses_a_threshold_outside_the_contract_interval() {
        // `NAN` is reported as its own case: an error carrying it could not be
        // compared to an identical copy of itself, so the variant that would
        // carry it must not exist.
        assert_eq!(
            SemanticPolicy::new(f64::NAN, 0.05),
            Err(SemanticError::NonFiniteThreshold)
        );
        assert_eq!(
            SemanticPolicy::new(f64::INFINITY, 0.05),
            Err(SemanticError::NonFiniteThreshold)
        );
        assert_eq!(
            SemanticPolicy::new(1.5, 0.05),
            Err(SemanticError::InvalidThreshold { threshold: 1.5 })
        );
        assert_eq!(
            SemanticPolicy::new(0.7, f64::NAN),
            Err(SemanticError::NonFiniteMargin)
        );
        assert_eq!(
            SemanticPolicy::new(0.7, 2.0),
            Err(SemanticError::InvalidMargin { margin: 2.0 })
        );
        assert!(SemanticPolicy::new(0.7, 0.05).is_ok());
        assert!(
            (SemanticPolicy::DEFAULT.threshold() - 0.72).abs() < 1e-12,
            "the shipped threshold is the declared one"
        );
        assert_eq!(SemanticPolicy::default(), SemanticPolicy::DEFAULT);
    }

    #[test]
    fn the_default_embed_batch_preserves_input_order() -> Result<(), Box<dyn std::error::Error>> {
        let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;

        let batch = embedder
            .embed_batch(&["Cancel", "the usual"])
            .map_err(|_| "the stub was configured not to fail")?;

        let expected = [vec![0.0_f32, 1.0], vec![1.0, 0.0]];
        assert_eq!(batch.len(), expected.len(), "batch length changed");
        for (actual, want) in batch.iter().zip(expected.iter()) {
            assert_vector(actual, want);
        }
        assert_eq!(calls.get(), 2, "the default embeds one text at a time");
        Ok(())
    }
}
