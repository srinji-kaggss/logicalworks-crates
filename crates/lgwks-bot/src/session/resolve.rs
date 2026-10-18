//! Resolving an utterance to an option: the resolver trait, its verdict and provenance. Split from `session.rs` (#526).

use super::{Deserialize, EmbedderIdentity, Hasher, Ordering, Question, Serialize, fmt};
// Named by an intra-doc link only.
#[cfg(doc)]
use super::DecisionReceipt;

/// Free-text option resolver seam.
pub trait Resolver {
    /// Resolves an utterance against the question in front of the person.
    ///
    /// Returns a [`Verdict`] — the [`Resolution`] together with the policy and
    /// model that produced it — and never an `Option<usize>`. The two-way form
    /// collapses *nothing matched* and *several matched equally well* into one
    /// `None`, and the second is the dangerous one: an answer that fits two
    /// options as well as each other is reported as unrecognized, so the
    /// session re-asks the full list and the identical answer resolves the
    /// identical way forever. A caller that cannot see the tie cannot narrow.
    ///
    /// The provenance is part of the return value rather than a second call,
    /// because *this score came from that model under that rule* is one fact.
    /// A caller that has to ask a resolver afterwards what it just did is
    /// re-deriving a fact it was already handed, and the derivation is only as
    /// good as the resolver's willingness to keep answering the same way.
    /// A question is more than its option list — see [`Question`] for why the
    /// identity travels with the options.
    fn resolve(&self, utterance: &str, question: &Question<'_>) -> Verdict;
}

/// The identity of the decision policy that produced a verdict.
///
/// Two fields rather than one string, because they answer different questions.
/// `label` names the tier whose parameters were in force, so a reader knows
/// what kind of decision this was before reading any score. `revision` is a
/// digest over the parameters themselves, so a threshold that moved is a
/// different version **without anyone remembering to bump a number by hand**:
/// a hand-bumped version is a promise, and a receipt cannot rest on a promise.
///
/// The parameters are hashed by their bit patterns rather than by their decimal
/// renderings, so two thresholds differing in the last bit are two versions and
/// no rounding stands between the value used and the value recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct PolicyVersion {
    /// The tier whose policy was in force, such as `lexicon` or `semantic`.
    pub(super) label: String,
    /// Content digest over the parameters in force, as lowercase hex.
    pub(super) revision: String,
}

impl PolicyVersion {
    /// Versions a policy from its label and the parameters in force.
    ///
    /// The label is hashed in as well, so two tiers that happen to declare the
    /// same parameters still have different revisions: a revision alone
    /// identifies one policy, which is what lets it be compared without reading
    /// the label beside it.
    #[must_use]
    pub fn new(label: impl Into<String>, parameters: &[f64]) -> Self {
        let label = label.into();
        let mut hasher = Hasher::new();
        hasher.update(label.as_bytes());
        for parameter in parameters {
            hasher.update(&parameter.to_bits().to_le_bytes());
        }
        Self {
            revision: hasher.finalize().to_hex(),
            label,
        }
    }

    /// Returns the tier label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the content digest over the policy's parameters.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

impl fmt::Display for PolicyVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.label, self.revision)
    }
}

/// What produced a verdict: the policy in force, and the model if there was one.
///
/// The two travel together because they are one fact. A receipt that named a
/// score and a tier but not the rule that scored it cannot answer the question
/// a score actually raises, which is *would this have decided the same way
/// yesterday*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct Provenance {
    /// The decision policy in force when the verdict was reached.
    pub(super) policy: PolicyVersion,
    /// The model consulted, when the verdict came from one.
    pub(super) model: Option<EmbedderIdentity>,
}

impl Provenance {
    /// The deterministic tiers decided; no model was consulted.
    #[must_use]
    pub fn without_model(policy: PolicyVersion) -> Self {
        Self {
            policy,
            model: None,
        }
    }

    /// A model was consulted under a policy, whether or not it answered.
    ///
    /// A degraded verdict names its model deliberately. [`DegradedReason`] says
    /// the resolver never reached a verdict, and *which* model was unavailable
    /// is exactly what the repair needs; a record that dropped the identity on
    /// the failure path would describe the one case an operator has to act on
    /// as though no model had been involved.
    #[must_use]
    pub fn with_model(policy: PolicyVersion, model: EmbedderIdentity) -> Self {
        Self {
            policy,
            model: Some(model),
        }
    }

    /// Returns the policy in force.
    #[must_use]
    pub fn policy(&self) -> &PolicyVersion {
        &self.policy
    }

    /// Returns the model consulted, or `None` when none was.
    ///
    /// `None` is a finding rather than a missing value: the deterministic tiers
    /// answered, and naming a model here would claim a comparison that never
    /// happened.
    #[must_use]
    pub fn model(&self) -> Option<&EmbedderIdentity> {
        self.model.as_ref()
    }
}

/// A resolver's answer: the verdict, and what produced it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Verdict {
    /// The three-way verdict.
    pub(super) resolution: Resolution,
    /// The policy and model behind it.
    pub(super) provenance: Provenance,
}

impl Verdict {
    /// Pairs a resolution with what produced it.
    #[must_use]
    pub const fn new(resolution: Resolution, provenance: Provenance) -> Self {
        Self {
            resolution,
            provenance,
        }
    }

    /// Returns the verdict.
    #[must_use]
    pub const fn resolution(&self) -> &Resolution {
        &self.resolution
    }

    /// Returns what produced the verdict.
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Consumes the verdict and returns the resolution alone.
    #[must_use]
    pub fn into_resolution(self) -> Resolution {
        self.resolution
    }
}

/// Order two scored candidates best-first, lowest index first on a tie.
///
/// The one comparator for the crate's two rankers — `language`'s lexicon and
/// `semantic`'s embedder — because they have to agree. A candidate the lexicon
/// placed above a tie must not be placed below one by the embedder, or the tier
/// decides the order rather than the score. Both callers feed it ascending
/// indices into a *stable* sort, so an equal score keeps lowest-index-first with
/// no tiebreak of its own.
///
/// A score with no order — a NaN on either side, the one pair `f64` cannot
/// compare — sorts **last**. `total_cmp` alone would rank it above every number,
/// handing the win to the one candidate that measured nothing, and treating it
/// as a tie would leave an unorderable entry in the list the caller then computes
/// its lead over. Neither ranker can produce a NaN today: both score through a
/// checked metric that refuses a degenerate vector, so this arm is a property of
/// the comparator rather than a branch anyone has taken.
pub(crate) fn by_score_descending(
    left: &(usize, MatchTier, f64),
    right: &(usize, MatchTier, f64),
) -> Ordering {
    match (left.2.is_nan(), right.2.is_nan()) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => right.2.total_cmp(&left.2),
    }
}

/// Which match tier produced a resolution.
///
/// Reported rather than inferred, because it is what makes a wrong resolution
/// explicable and therefore repairable. An `Exact` miss is a learned alias
/// pointing at the wrong option; a `Phonetic` miss is the English bias of
/// [`crate::language::phonetic_key`]; a `Fuzzy` miss is a threshold. Three
/// different repairs, and a bare `usize` names none of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MatchTier {
    /// The utterance matched exactly, directly or through a learned alias.
    Exact,
    /// The utterance and the option reduce to the same sound-alike key.
    Phonetic,
    /// The utterance and the option are lexically close.
    Fuzzy,
    /// The utterance and the option are close in a model's embedding space.
    Semantic,
}

/// Why a resolver could not reach a verdict.
///
/// A closed set rather than a string, so a caller can branch on the cause
/// without parsing prose, and so the set of ways a resolver can fail is stated
/// where a reviewer reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DegradedReason {
    /// The semantic tier's embedder returned an error, or a vector of the
    /// wrong length.
    EmbedderUnavailable,
    /// An embedding came back as a value no angle can be computed from, so a
    /// comparison the decision needs was never made.
    ///
    /// The embedder answered, and its answer is not a direction: an all-zero
    /// vector, or one carrying `NaN` or an infinity. That is a different
    /// failure from [`Self::EmbedderUnavailable`] and has a different repair —
    /// nothing is down, one of the vectors is degenerate — which is why it is
    /// not folded into it.
    ///
    /// The finer cause is carried by [`lgwks_std::similarity::CosineError`],
    /// which separates a zero magnitude from a non-finite one. It is not
    /// repeated here because the routing is identical: a comparison set missing
    /// even one member cannot produce the `Resolved` or `Absent` verdict a
    /// complete set produces, so the session re-asks and records the cause
    /// whichever of the two it was.
    UnmeasurableEmbedding,
}

impl fmt::Display for DegradedReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::EmbedderUnavailable => formatter.write_str("the embedder is unavailable"),
            Self::UnmeasurableEmbedding => {
                formatter.write_str("an embedding could not be measured")
            }
        }
    }
}

/// The outcome of resolving an utterance against the candidate options.
///
/// Serializable, and not only for convenience: this is the shape a
/// [`DecisionReceipt`] carries verbatim, so a receipt cannot hold a verdict the
/// journal could not write down.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Resolution {
    /// One option matched and led the runner-up by the required margin.
    Resolved {
        /// Index of the selected option.
        index: usize,
        /// The tier that produced the match.
        tier: MatchTier,
        /// The match score.
        score: f64,
        /// The lead held over the runner-up *within the winning tier*.
        ///
        /// Within, not across: tier scores are not comparable, and a resolver
        /// that applied precedence has already excluded every lower tier from
        /// the comparison. A `lead` of `1.0` on a lone exact candidate therefore
        /// says "nothing in the exact tier contested this", not "nothing at all
        /// came close".
        lead: f64,
    },
    /// Options matched but none led by the required margin.
    ///
    /// The caller narrows its re-ask to [`Self::Ambiguous::tied`] rather than
    /// repeating the full list. This is the state `Option<usize>` cannot
    /// express, and the one that made a repeated ambiguous answer a loop.
    Ambiguous {
        /// Every option index still in play, lowest first.
        tied: Vec<usize>,
        /// The tier whose candidates tied.
        ///
        /// Reported for the reason [`Self::Resolved::tier`] is: two exact
        /// candidates that fold to one normalized form, two phonetic candidates
        /// that share a sound-alike key, and two fuzzy candidates inside a
        /// margin are three different ties with three different repairs, and a
        /// bare list of indices names none of them.
        tier: MatchTier,
        /// The highest score observed.
        score: f64,
    },
    /// No option matched.
    Absent {
        /// The highest score observed.
        best_score: f64,
    },
    /// The person used a phrase whose confirmed meaning has been withdrawn.
    ///
    /// Distinct from [`Self::Absent`] in the same way [`Self::Degraded`] is, and
    /// for the same reason: the two render identically in a transcript and need
    /// different repairs. `Absent` says the person's words did not fit the
    /// options and a rephrase may help. `StaleAlias` says the words *did* have a
    /// confirmed meaning, at this question, and the option it was bound to is no
    /// longer offered — so the answer is not a rephrase but a conversation, and
    /// the operator reading the record is the one who can have it.
    ///
    /// Returning this rather than a fuzzy reading is the whole point: the
    /// binding is authority a person granted, and re-spending it on whatever now
    /// occupies the old option's position would be using a confirmation the
    /// person never gave. The persona's own reading of the phrase still stands
    /// when the current list produces one; this verdict appears where the
    /// superseded binding is the only thing that would have matched.
    StaleAlias {
        /// The question whose vocabulary the confirmation was made in.
        question: String,
        /// The option text the confirmation bound, which is no longer offered.
        option: String,
    },
    /// The resolver could not reach a verdict at all.
    ///
    /// This is [`Self::Absent`]'s neighbour and not a spelling of it. `Absent`
    /// means the options were all considered and none fit: the person was not
    /// understood, and asking again in different words can help. `Degraded`
    /// means the resolver never got to consider them: a dependency it needs is
    /// unavailable, or answered with a value no comparison can be drawn from,
    /// and asking the person again cannot help — the same utterance will
    /// degrade identically.
    ///
    /// Carries no `best_score`, deliberately. There is no score; a failed
    /// resolver observed no similarity, and reporting `0.0` would be a
    /// measurement that was never taken. It is the same two-valued record
    /// [`Self::Ambiguous`] exists to remove — *nothing matched* versus *may have
    /// matched, and we could not look* — arriving a fourth time.
    ///
    /// That is also why an incomplete comparison set degrades instead of
    /// deciding: a winner measured against fewer competitors than the list
    /// holds is a winner over a field that was never fully looked at, and the
    /// margin it reports would be the whole of its own score.
    Degraded {
        /// The cause, for the caller to record and route on.
        reason: DegradedReason,
    },
}
