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
//! The identity travels out on the verdict itself, as
//! [`Provenance::model`], and not only into
//! this resolver's own accessor. `resolve` calls `identity()` once per decision
//! and the caller holds the result: a caller that resolved through a trait
//! object has no way back to `embedder_identity`, and `embedder_identity` is a
//! property of *this* resolver rather than of the decision it just made.
//!
//! # Failure is a verdict, and the record is the journal
//!
//! An embedder can fail, and a failed embedder must not be reported as *nothing
//! matched*. The two need different repairs — rephrase, versus restore the
//! dependency — and they would otherwise render identically. This tier therefore
//! returns [`Resolution::Degraded`], and a caller that does not distinguish it
//! still re-asks, which is the safe direction.
//!
//! A degraded verdict still names its model, deliberately. The model is what was
//! expected to answer and did not, so a record of the failure that omits it
//! cannot be reproduced — and `Degraded` is precisely the verdict a reader will
//! later want to attribute.
//!
//! An embedder can also answer with something no comparison can be drawn from: a
//! zero vector, or one carrying a non-finite value. That is not a failure of the
//! dependency and it is not an absence of meaning either — it is a comparison
//! that was never made, and a decision over a set missing a member is not the
//! decision the option list calls for. So an unusable vector anywhere in the set
//! — the utterance's or a competitor's — degrades the whole verdict rather than
//! dropping the candidate. Dropping it would let a survivor claim a lead over a
//! competitor nobody measured, and scoring it as zero would assert a similarity
//! that was never observed. There is no [`Resolution`] variant carrying partial
//! evidence, and a partial result is not worth inventing one for.
//!
//! This module does not log. It reports. The cause travels to the caller in the
//! verdict, and the verdict reaches the run's declared record — the
//! [`DecisionReceipt`] written to the
//! [`Journal`] — rather than a session transcript,
//! which is a rendering of what was said and not of what was decided. A log line
//! would be a second, unmanaged copy of the same fact, and this crate has no
//! logging edge to write one through. An [`Embedder`] that fails is expected to
//! describe its own failure — it owns its error type and the context around it —
//! while the resolver's job is the closed verdict the flow can route on.
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

use lgwks_std::json::{Deserialize, Serialize};
use lgwks_std::similarity::{Cosine, CosineError};

use crate::language::{Alias, LanguageResolver, decide};
use crate::session::{
    AnswerDomain, DegradedReason, MatchTier, PolicyVersion, Provenance, Question, Resolution,
    Resolver, Verdict, by_score_descending,
};

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

    /// Returns the identity that travels with every decision this embedder
    /// informs.
    ///
    /// Called once per decision by [`SemanticResolver::resolve`], which puts the
    /// result on the [`Verdict`] it returns, so the identity reaches the caller
    /// and the decision receipt without the caller holding this embedder.
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
///
/// The three fields are serializable because they are carried in a decision
/// receipt: a receipt is exported from a run and read in another process, and a
/// provenance field that does not survive that trip is not a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
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
            let refusal = Err(SemanticError::UnnamedEmbedder);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if digest.is_empty() {
            let refusal = Err(SemanticError::UndigestedEmbedder { name });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if dimension == 0 {
            let refusal = Err(SemanticError::ZeroDimension { name });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
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
/// Names the tier whose parameters a [`PolicyVersion`] digests.
///
/// Distinct from [`crate::language`]'s label: the two tiers are independently
/// versioned, and a receipt naming `semantic` is a claim about the cosine
/// threshold rather than about the lexicon's blend.
const SEMANTIC_LABEL: &str = "semantic";

/// The acceptance policy for the semantic tier: the cosine at or above which an
/// option is a candidate, and the lead it must hold.
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
            let refusal = Err(SemanticError::NonFiniteThreshold);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if !(0.0..=1.0).contains(&threshold) {
            let refusal = Err(SemanticError::InvalidThreshold { threshold });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if !margin.is_finite() {
            let refusal = Err(SemanticError::NonFiniteMargin);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if !(0.0..=1.0).contains(&margin) {
            let refusal = Err(SemanticError::InvalidMargin { margin });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
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
/// `Debug` is manual rather than derived; see the impl for why the injected
/// model is not part of the rendering.
#[non_exhaustive]
pub struct SemanticResolver<E> {
    /// The deterministic tiers, consulted first and authoritatively.
    lexicon: LanguageResolver,
    /// The injected model.
    embedder: E,
    /// The acceptance policy for the semantic tier.
    policy: SemanticPolicy,
}

impl<E> core::fmt::Debug for SemanticResolver<E> {
    /// Manual rather than derived, and not because the state is unsuitable: a
    /// derive would attach an `E: Debug` bound, making `Debug` unavailable for
    /// exactly the embedders a consumer is most likely to supply, since
    /// [`Embedder`] does not require it. The lexicon and the policy are printed
    /// because they are this crate's own state; the model is the caller's, so
    /// the rendering stops at `finish_non_exhaustive` rather than demanding
    /// that every embedder be printable.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SemanticResolver")
            .field("lexicon", &self.lexicon)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
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

    /// Returns the identity of the model this resolver was built around.
    ///
    /// A property of the resolver, not a record of anything it decided: the
    /// identity of the model behind a *particular* decision is on that
    /// decision's [`Verdict`], because only the verdict is reachable from a
    /// caller that holds a `Box<dyn Resolver>`. Use this to inspect what a
    /// resolver was constructed with; use the verdict — or the decision receipt
    /// it reaches — to attribute a decision.
    #[must_use]
    pub fn embedder_identity(&self) -> &EmbedderIdentity {
        self.embedder.identity()
    }

    /// Returns the acceptance policy in force.
    #[must_use]
    pub const fn policy(&self) -> SemanticPolicy {
        self.policy
    }

    /// Returns the content identity of the acceptance policy in force.
    ///
    /// Derived from the policy's own numbers, not written down beside them, so
    /// that retuning the threshold in a later release changes every receipt the
    /// semantic tier produces. A hand-kept revision string is a claim that can
    /// drift; a digest of the parameters is the parameters.
    #[must_use]
    pub fn policy_version(&self) -> PolicyVersion {
        PolicyVersion::new(
            SEMANTIC_LABEL,
            &[self.policy.threshold(), self.policy.margin()],
        )
    }

    /// Records a confirmed alias on the deterministic tier.
    ///
    /// Delegated rather than reimplemented: an alias is a lexicon fact, and a
    /// person who confirms a resolution has taught the deterministic tier
    /// something it will answer from without a model. That is the direction of
    /// travel this module wants — a confirmed correction should move a phrase
    /// *out* of the semantic tier and into the reproducible one.
    ///
    /// Takes the question scope for the reason [`crate::language::Alias`] exists:
    /// a confirmation is a fact about one question's vocabulary, and the model
    /// below is not a licence to carry it to another.
    pub fn learn(&mut self, question: &str, utterance: &str, option: &str) -> Option<Alias> {
        self.lexicon.learn(question, utterance, option)
    }

    /// Removes one question's learned alias, returning the row removed.
    pub fn forget(&mut self, question: &str, utterance: &str) -> Option<Alias> {
        self.lexicon.forget(question, utterance)
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

    /// Maps a refused comparison to the reason the whole decision degrades.
    ///
    /// Every refusal degrades, including one this crate has never seen:
    /// `CosineError` is `#[non_exhaustive]`, so a future variant lands on the
    /// final arm and stops the decision rather than being dropped. Dropping the
    /// candidate is the tempting reading — one option the tier cannot speak
    /// about, with the rest still comparable — and it is the defect: the
    /// decision then runs over a field one member short, and a surviving winner
    /// reports a lead over a competitor that was never measured. Issue #31 is
    /// the same defect reached through the threshold instead of through an
    /// error, and a fallback arm is exactly how it comes back.
    fn degrade(reason: CosineError) -> DegradedReason {
        match reason {
            // The provider contradicted its own declared identity, so the
            // tier's input contract is what broke rather than one value.
            CosineError::DimensionMismatch { .. } => DegradedReason::EmbedderUnavailable,
            // A vector with no computable direction: zero magnitude, or a
            // value that is not finite. The comparison was not made.
            _ => DegradedReason::UnmeasurableEmbedding,
        }
    }

    /// Refuses a vector no angle can be computed from.
    ///
    /// The check runs the metric against the vector itself rather than
    /// restating the rule: a vector carrying a direction scores exactly `1.0`
    /// against itself, and one that does not is refused by
    /// [`Cosine::try_score`] for the same reason a real comparison would. Two
    /// definitions of "measurable" would be free to disagree.
    fn ensure_measurable(metric: &Cosine, vector: &[f32]) -> Result<(), DegradedReason> {
        match metric.try_score(vector, vector) {
            Ok(_) => Ok(()),
            Err(reason) => Err(Self::degrade(reason)),
        }
    }

    /// Scores every option against the utterance, best-first.
    ///
    /// Every option whose vector can be compared is measured and returned,
    /// including one that falls below the acceptance threshold. The threshold
    /// is applied by [`decide`], not here, because it decides which option may
    /// *win*: applying it here would delete the runner-up before the lead over
    /// it was measured, and a winner whose only competitor was discarded reports
    /// the whole of its score as a lead it never held.
    ///
    /// An incomplete comparison set is not scored at all. If any embedding —
    /// the utterance's or a candidate's — is one no angle can be drawn from,
    /// the decision degrades, because the alternatives are worse: skipping the
    /// candidate lets a survivor claim a lead over an unmeasured field, and
    /// scoring the degenerate vector as zero asserts a similarity that was
    /// never observed. The set is all-or-nothing on purpose; there is no
    /// `Resolution` variant that carries partial evidence, so a partial result
    /// cannot be reported honestly and is not reported at all.
    fn score_semantically(
        &self,
        utterance: &str,
        options: &[String],
    ) -> Result<Vec<(usize, MatchTier, f64)>, DegradedReason> {
        let metric = Cosine::new();
        let target = self.embed(utterance)?;
        // Checked before the loop as well as inside it, so a degenerate
        // utterance degrades even when there is no candidate to compare it
        // against and no comparison would otherwise be attempted.
        Self::ensure_measurable(&metric, &target)?;

        let mut scored: Vec<(usize, MatchTier, f64)> = Vec::with_capacity(options.len());
        for (index, option) in options.iter().enumerate() {
            let candidate = self.embed(option)?;
            // The utterance is known to carry a direction from the check
            // above, so a refusal here can only be about this candidate.
            match metric.try_score(&target, &candidate) {
                Ok(score) => scored.push((index, MatchTier::Semantic, score)),
                Err(reason) => {
                    let refusal = Err(Self::degrade(reason));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "score_semantically: returning an error to the caller");
                    return refusal;
                }
            }
        }

        // Best-first through the crate's one comparator, the same one the lexicon
        // ranks with: the input is in ascending index order into a stable sort,
        // so a tie keeps lowest-index-first.
        scored.sort_by(by_score_descending);
        Ok(scored)
    }
}

impl<E: Embedder> Resolver for SemanticResolver<E> {
    fn resolve(&self, utterance: &str, question: &Question<'_>) -> Verdict {
        let deterministic = self.lexicon.decide_for(utterance, question);
        // A numeric question never reaches the model, at all. The lexicon's
        // integer path answers by decoded value and reports `Absent` for an
        // answer that no option holds — and that `Absent` must stay `Absent`.
        // Handing it to an embedding comparison is how a model would pick
        // whichever numeral *looks* closest to a number nobody offered, which is
        // the same sign-erasing guess the typed path exists to remove, arrived
        // at from the other end.
        if question.domain() == AnswerDomain::Integer {
            // The lexicon decided, so the lexicon's policy is what decided and
            // no model was consulted — the same attribution the branch below
            // makes for every other lexicon answer.
            return Verdict::new(
                deterministic,
                Provenance::without_model(self.lexicon.policy_version()),
            );
        }
        if !matches!(deterministic, Resolution::Absent { .. }) {
            // The lexicon answered, so the lexicon's policy is what decided and
            // no model was consulted. Naming this resolver's model here would
            // attribute a reproducible verdict to a model that never ran.
            return Verdict::new(
                deterministic,
                Provenance::without_model(self.lexicon.policy_version()),
            );
        }
        // The lexicon considered every option and none fit. This is the only
        // path on which a model is consulted, so no verdict the lexicon reached
        // can be changed by enabling this tier.
        let provenance =
            Provenance::with_model(self.policy_version(), self.embedder.identity().clone());
        match self.score_semantically(utterance, question.options()) {
            Ok(scored) => Verdict::new(
                decide(&scored, self.policy.threshold(), self.policy.margin()),
                provenance,
            ),
            Err(reason) => Verdict::new(Resolution::Degraded { reason }, provenance),
        }
    }
}

#[cfg(test)]
mod tests;
