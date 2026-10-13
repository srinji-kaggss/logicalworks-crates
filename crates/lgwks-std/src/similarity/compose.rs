//! Weighted compositions of scorers: the plain Weighted adapter, and CheckedEvidence, which keeps each component's refusal distinct from a measured zero.

use core::fmt;

use super::kernel::unit_score;
use super::{ComponentOutcome, Cosine, EditDistance, EvidenceError, Geometry, Similarity};

/// Why a weighted scorer could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// A checked composition's scorer and weight lists had different lengths.
    WeightCountMismatch {
        /// Number of configured scorers.
        scorers: usize,
        /// Number of configured weights.
        weights: usize,
    },
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
            Self::WeightCountMismatch { scorers, weights } => write!(
                formatter,
                "weighted similarity has {scorers} scorers and {weights} weights"
            ),
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

/// Refuses a weighted scorer built from nothing.
///
/// Shared by both constructors so the empty case has one refusal and one place
/// it is reported from.
fn require_present(count: usize) -> Result<(), WeightedError> {
    if count == 0 {
        let refusal = Err(WeightedError::Empty);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "weighted scorer: there are no components");
        return refusal;
    }
    Ok(())
}

/// Refuses an acceptance threshold or weight set outside the declared policy:
/// the threshold must lie in `[0, 1]`, each weight must be finite and
/// non-negative, and the weights may not sum past one.
///
/// Shared by both constructors, so the plain and the checked scorer cannot come
/// to disagree about which policies are legal. The threshold is judged before
/// the weights, as both constructors always did.
fn validate_policy(
    threshold: f64,
    weights: impl Iterator<Item = f64>,
) -> Result<(), WeightedError> {
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        let refusal = Err(WeightedError::InvalidThreshold);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), threshold, "weighted scorer: the threshold is outside [0, 1]");
        return refusal;
    }
    let mut total = 0.0;
    for (index, weight) in weights.enumerate() {
        if !weight.is_finite() || weight < 0.0 {
            let refusal = Err(WeightedError::InvalidWeight { index });
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), index, weight, "weighted scorer: the weight is not finite and non-negative");
            return refusal;
        }
        total += weight;
    }
    if total > 1.0 {
        let refusal = Err(WeightedError::WeightSumExceedsOne);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), total, "weighted scorer: the weights sum past one");
        return refusal;
    }
    Ok(())
}

impl<Value: ?Sized> Weighted<Value> {
    /// Creates a weighted scorer and its acceptance threshold.
    pub fn new(
        components: Vec<(f64, Box<dyn Similarity<Value = Value>>)>,
        threshold: f64,
    ) -> Result<Self, WeightedError> {
        require_present(components.len())?;
        validate_policy(threshold, components.iter().map(|component| component.0))?;

        Ok(Self {
            components,
            threshold,
        })
    }

    /// Returns the acceptance threshold.
    #[must_use]
    pub const fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Scores the pair and reports whether it reaches the configured threshold.
    ///
    /// This is the infallible adapter: a refused component reaches it as `0.0`,
    /// so an unavailable component satisfies a threshold of `0.0`. Prefer
    /// [`CheckedEvidence::verdict`], which retains the refusal.
    #[must_use]
    pub fn is_accepted(&self, left: &Value, right: &Value) -> bool {
        self.score(left, right) >= self.threshold
    }

    /// Returns the sum of the configured component weights.
    ///
    /// A total below `1.0` is a deliberate evidence deficit: the composition's
    /// maximum is that total, not `1.0`, and it is never renormalized away.
    #[must_use]
    pub fn total_weight(&self) -> f64 {
        self.components
            .iter()
            .fold(0.0, |sum, component| sum + component.0)
    }
}

impl<Value: ?Sized> core::fmt::Debug for Weighted<Value> {
    /// Reports the composition rather than the components.
    ///
    /// The component list is a set of `dyn Similarity` trait objects, so a
    /// derived `Debug` would print pointers instead of policy: the useful
    /// facts are how many scorers there are and where the acceptance line
    /// sits.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Weighted")
            .field("components", &self.components.len())
            .field("weights", &WeightList(self))
            .field("threshold", &self.threshold)
            .finish_non_exhaustive()
    }
}

/// The component weights, formatted in composition order.
struct WeightList<'a, Value: ?Sized>(&'a Weighted<Value>);

impl<Value: ?Sized> core::fmt::Debug for WeightList<'_, Value> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list()
            .entries(self.0.components.iter().map(|component| component.0))
            .finish()
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

/// A scorer that reports a refusal instead of folding it into a number.
///
/// [`Similarity`] is the numeric seam every scorer can satisfy; this is the
/// checked seam a scorer that can refuse also satisfies, so a composition can
/// carry the refusal rather than losing it at a trait-object boundary.
///
/// [`Clone`] is a supertrait rather than a bound on the boxed trait object
/// because Rust permits only auto traits there. The requirement is real
/// rather than convenience: [`CheckedEvidence`] is clonable so a caller can
/// hand each tenant its own policy, and a component that could not be copied
/// would make that impossible to state.
pub trait CheckedSimilarity {
    /// The value type accepted by this scorer.
    ///
    /// [`Sync`] because [`CheckedEvidence`] is shareable across threads and a
    /// scorer that were not would make that a lie: the composition reads its
    /// components from `&self` on every call.
    type Value: ?Sized + Sync;

    /// Scores the pair, or reports why it could not be measured.
    ///
    /// # Errors
    ///
    /// Returns [`EvidenceError`] naming the reason the measurement is
    /// unavailable. Every refusal is a variant of that enum; none of them is a
    /// score.
    fn try_score(&self, left: &Self::Value, right: &Self::Value) -> Result<f64, EvidenceError>;
}

impl CheckedSimilarity for EditDistance {
    type Value = str;

    fn try_score(&self, left: &Self::Value, right: &Self::Value) -> Result<f64, EvidenceError> {
        EditDistance::try_score(self, left, right).map_err(EvidenceError::from)
    }
}

impl CheckedSimilarity for Cosine {
    type Value = [f32];

    /// Returns the raw `[-1, 1]` cosine, not the normalized mapping, so a
    /// caller reading a weighted composition sees the domain it declared. Use
    /// [`Cosine::normalized_score`] for the unit interval.
    fn try_score(&self, left: &Self::Value, right: &Self::Value) -> Result<f64, EvidenceError> {
        Cosine::try_score(self, left, right).map_err(EvidenceError::from)
    }
}

impl CheckedSimilarity for Geometry {
    type Value = [f64; 4];

    fn try_score(&self, left: &Self::Value, right: &Self::Value) -> Result<f64, EvidenceError> {
        Ok(Similarity::score(self, left, right))
    }
}

/// A weighted composition that preserves each component's refusal.
///
/// This is the authority-facing path. [`Weighted`] stays available for callers
/// that want a bare number and accept that a refusal reads as `0.0`;
/// [`CheckedEvidence`] keeps "could not measure" distinct from "measured
/// zero" all the way to the acceptance decision.
///
/// # Sharing one policy across threads
///
/// One `CheckedEvidence` serves any number of concurrent callers, and
/// [`verdict`] takes `&self`. That is only true because the policy is
/// immutable, so the contract is carried on the types rather than left to
/// inference:
///
/// - [`CheckedSimilarity::Value`] is [`Sync`], and [`CheckedEvidence::new`]
///   takes `Box<dyn CheckedSimilarity<Value = Value> + Send + Sync>`.
/// - Every scorer shipped here is `Copy` with no interior mutability, so the
///   bounds are satisfied without a lock.
/// - [`CheckedSimilarity::Value`] being [`Sync`] is what lets a caller hold a
///   `&str` or a `&[f32]` across the concurrent calls it shares.
///
/// A caller whose own scorer is not `Sync` still composes: the trait carries
/// the bound, so such a scorer cannot be installed rather than silently
/// producing a policy that is `Sync` on the outside and racy inside.
///
/// The per-tenant route is to build one composition per tenant rather than to
/// share one. Two [`CheckedEvidence`] values built from the same scorer type are
/// already two independent configurations, and that is the shape
/// `tests/sim_tenant_isolation.rs` drives across three tenants at every
/// concurrency tier.
///
/// [`verdict`]: CheckedEvidence::verdict
///
/// ```
/// use lgwks_std::similarity::{CheckedEvidence, CheckedSimilarity, EditDistance};
///
/// let policy = CheckedEvidence::new(
///     vec![Box::new(EditDistance::new(3))],
///     vec![1.0],
///     0.0,
/// )?;
/// // The input is over budget, so it is refused rather than scored 0.0.
/// let verdict = policy.verdict("four", "four")?;
/// assert!(!verdict.is_accepted());
/// assert_eq!(verdict.score(), None);
/// assert_eq!(verdict.refusals()[0].index(), 0);
/// // One policy, shared: the bound that makes it shareable is part of the API.
/// fn shared<T: Sync>(value: T) -> std::sync::Arc<T> {
///     std::sync::Arc::new(value)
/// }
/// let shared = shared(policy);
/// /// // Within the three-character budget, so the same policy measures rather than
/// // refuses, and one `Arc` hands the identical value to any number of callers.
/// assert!(shared.verdict("cat", "cat")?.is_accepted());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct CheckedEvidence<Value: ?Sized> {
    /// The component scorers, in configuration order.
    scorers: Vec<Box<dyn CheckedSimilarity<Value = Value> + Send + Sync>>,
    /// Each component's bounded non-negative weight, parallel to `scorers`.
    weights: Vec<f64>,
    /// The score at or above which a measured verdict accepts.
    threshold: f64,
}

impl<Value: ?Sized + Sync> CheckedEvidence<Value> {
    /// Creates a checked composition from parallel scorer and weight lists.
    ///
    /// The two lists must be the same length. Weights are validated exactly as
    /// [`Weighted::new`] validates them: finite, non-negative, totalling at
    /// most `1.0`. A total of `0.0` is accepted here and refused at evaluation
    /// time as [`EvidenceError::InsufficientEvidence`], so the policy can be
    /// constructed and inspected while still never producing a verdict from
    /// nothing.
    ///
    /// # Errors
    ///
    /// Returns [`WeightedError::Empty`] for no components,
    /// [`WeightedError::InvalidWeight`] for a negative or non-finite weight,
    /// [`WeightedError::WeightSumExceedsOne`] for a total above `1.0`, and
    /// [`WeightedError::InvalidThreshold`] for a threshold outside `[0, 1]`.
    pub fn new(
        scorers: Vec<Box<dyn CheckedSimilarity<Value = Value> + Send + Sync>>,
        weights: Vec<f64>,
        threshold: f64,
    ) -> Result<Self, WeightedError> {
        require_present(scorers.len())?;
        if scorers.len() != weights.len() {
            let refusal = Err(WeightedError::WeightCountMismatch {
                scorers: scorers.len(),
                weights: weights.len(),
            });
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), scorers = scorers.len(), weights = weights.len(), "checked weighted scorer: every scorer needs exactly one weight");
            return refusal;
        }
        validate_policy(threshold, weights.iter().copied())?;
        Ok(Self {
            scorers,
            weights,
            threshold,
        })
    }

    /// Returns the acceptance threshold.
    #[must_use]
    pub const fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Returns the sum of the configured component weights.
    #[must_use]
    pub fn total_weight(&self) -> f64 {
        self.weights.iter().fold(0.0, |sum, weight| sum + weight)
    }

    /// Evaluates every component and returns the verdict.
    ///
    /// When any component refuses, the verdict's `score` is [`None`] and
    /// `is_accepted` is `false` at every threshold, including `0.0`: a refusal
    /// is not a measurement of zero. Each refusal is attributed to its
    /// component index and retains its typed reason.
    ///
    /// The surviving weights are not renormalized. A missing component leaves an
    /// evidence deficit rather than inflating its neighbours to cover the gap,
    /// so geometry alone can never acquire a missing identity field's authority.
    ///
    /// # Errors
    ///
    /// Returns [`EvidenceError::InsufficientEvidence`] when the weight total is
    /// zero: there is nothing to weigh, which is not a measurement of zero
    /// similarity.
    pub fn verdict(&self, left: &Value, right: &Value) -> Result<EvidenceVerdict, EvidenceError> {
        let total_weight = self.total_weight();
        if total_weight <= 0.0 {
            let refusal = Err(EvidenceError::InsufficientEvidence {
                total_weight: total_weight.to_bits(),
            });
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "verdict: returning an error to the caller");
            return refusal;
        }

        let mut outcomes = Vec::with_capacity(self.scorers.len());
        let mut measured_total = 0.0;
        let mut refusals = 0_usize;
        for (index, scorer) in self.scorers.iter().enumerate() {
            let weight = self.weights[index];
            match scorer.try_score(left, right) {
                Ok(score) => {
                    measured_total += weight * score;
                    outcomes.push(ComponentOutcome {
                        index,
                        weight,
                        score: Some(score),
                        reason: None,
                    });
                }
                Err(reason) => {
                    refusals = refusals.saturating_add(1);
                    outcomes.push(ComponentOutcome {
                        index,
                        weight,
                        score: None,
                        reason: Some(reason),
                    });
                }
            }
        }

        if refusals > 0 {
            return Ok(EvidenceVerdict {
                score: None,
                is_accepted: false,
                outcomes,
            });
        }
        let score = unit_score(measured_total);
        Ok(EvidenceVerdict {
            score: Some(score),
            is_accepted: score >= self.threshold,
            outcomes,
        })
    }
}

impl<Value: ?Sized + Sync> core::fmt::Debug for CheckedEvidence<Value> {
    /// Reports the composition policy rather than the scorer identities.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CheckedEvidence")
            .field("components", &self.scorers.len())
            .field("weights", &self.weights)
            .field("threshold", &self.threshold)
            .finish()
    }
}

/// A checked composition's verdict: what was measured, what refused, and
/// whether the pair is accepted.
///
/// The score is [`None`] whenever any component refused, so a refusal is never
/// read as a number, and the composition is then not accepted at any
/// threshold, including `0.0`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EvidenceVerdict {
    /// The composed score, or [`None`] when any component refused.
    score: Option<f64>,
    /// Whether the composition reaches the configured threshold.
    ///
    /// Always `false` when the score is [`None`].
    is_accepted: bool,
    /// Each component's index, weight, score, and refusal reason.
    outcomes: Vec<ComponentOutcome>,
}

impl EvidenceVerdict {
    /// Returns the composed score, or [`None`] when any component refused.
    ///
    /// [`None`] is the refusal signal: it is distinct from a measured `0.0`,
    /// and it never satisfies a threshold.
    #[must_use]
    pub const fn score(&self) -> Option<f64> {
        self.score
    }

    /// Whether the composition reaches the configured threshold.
    ///
    /// Always `false` when [`Self::score`] is [`None`].
    #[must_use]
    pub const fn is_accepted(&self) -> bool {
        self.is_accepted
    }

    /// Returns each component's outcome, in composition order.
    #[must_use]
    pub fn outcomes(&self) -> &[ComponentOutcome] {
        &self.outcomes
    }

    /// Returns only the components that refused, in composition order.
    #[must_use]
    pub fn refusals(&self) -> Vec<&ComponentOutcome> {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.reason.is_some())
            .collect()
    }
}
