//! Completion is a claim, and a claim needs evidence.
//!
//! A model saying "done" is a *string*. This module makes it a typed value that
//! can only be admitted with the evidence it names actually present, and it
//! separates three outcomes that are routinely reported as one:
//!
//! | Fact | [`CompletionOutcome`] |
//! |---|---|
//! | the claim named only evidence the run holds, and covered all of it | [`CompletionOutcome::Admitted`] |
//! | the claim named evidence the run does not hold | [`CompletionOutcome::NotEvidenced`] |
//! | the claim covered only part of what was asked for | [`CompletionOutcome::Incomplete`] |
//!
//! [`CompletionOutcome::NotEvidenced`] is the row's "model says done without
//! evidence" arm, and it is a *refusal with the missing references named*, not a
//! success and not a generic failure. A caller can therefore tell a model that
//! lied apart from a run that stopped short, which are two different repairs.
//!
//! The coverage half is the row's "truncated data never becomes a full-coverage
//! claim" arm. A payload carrying truncated data is admitted, but what it is
//! allowed to claim is [`Coverage::Partial`] — and that value cannot be
//! constructed as [`Coverage::Complete`] from a claim alone. See
//! [`Coverage::from_claim`].

use crate::journal::frame::SaturatingFrom;
use std::fmt;

use super::{Intervention, Outcome, Provenance, Refusal};

/// The most evidence references one completion claim may name.
///
/// A claim naming more references than this is refused whole rather than
/// truncated: a claim trimmed to its first `n` references would assert
/// completeness over the part it kept and silently drop the part that mattered.
pub const MAX_EVIDENCE_REFS: usize = 64;

/// How much of the work a completion claim says it covers.
///
/// The `Partial`/`Complete` split is a claim's own, and it is *not* something a
/// payload decides: [`Coverage::from_claim`] maps every unrecognized claim onto
/// [`Coverage::Partial`], so a model that says "complete" in any spelling this
/// decoder did not define is reporting the conservative value rather than the
/// convenient one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Coverage {
    /// Some of the work, and the claim says how much: `covered` out of `asked`.
    ///
    /// Carries both counts rather than a ratio, because a ratio of zero to zero
    /// is not a fact and a ratio of one to zero is a division a reader would have
    /// to guard.
    Partial {
        /// How many units of work the claim covers.
        covered: u64,
        /// How many units of work were asked for.
        asked: u64,
    },
    /// Every unit of work, and every evidence reference the claim named is
    /// present.
    ///
    /// Only [`CompletionOutcome::Admitted`] ever produces this, and only from a
    /// claim that passed the evidence check. There is no constructor that makes
    /// it from a coverage claim alone.
    Complete,
}

impl Coverage {
    /// Read a payload's `coverage` claim onto this type.
    ///
    /// `complete` is *not* mapped to [`Coverage::Complete`]. It maps to
    /// [`Coverage::Partial`] with the two counts equal, because at the point a
    /// payload is decoded nothing has established that the evidence is present;
    /// only [`Completion::admit`] can, and it is a separate call with the run's
    /// evidence in hand. This is the whole of "truncated data never becomes a
    /// full-coverage claim" in one function.
    #[must_use]
    pub fn from_claim(claim: Option<&str>) -> Self {
        match claim {
            Some("complete") => Self::Partial {
                covered: 1,
                asked: 1,
            },
            _ => Self::Partial {
                covered: 0,
                asked: 1,
            },
        }
    }

    /// Whether this coverage is a claim of full coverage.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }

    /// How many units of work this coverage covers, and how many were asked for.
    #[must_use]
    pub const fn counts(&self) -> (u64, u64) {
        match *self {
            Self::Partial { covered, asked } => (covered, asked),
            Self::Complete => (1, 1),
        }
    }

    /// The name of this arm, for a report.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match *self {
            Self::Partial { .. } => "Partial",
            Self::Complete => "Complete",
        }
    }
}

impl fmt::Display for Coverage {
    /// `covered/asked` for a partial claim, and `complete` for the full one.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Partial { covered, asked } => write!(formatter, "{covered}/{asked}"),
            Self::Complete => formatter.write_str("complete"),
        }
    }
}

/// What a completion claim asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CompletionKind {
    /// The work finished and the outputs are produced.
    Finished,
    /// The work is abandoned: the run will not produce its outputs.
    Abandoned,
}

impl CompletionKind {
    /// The name of this arm.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Finished => "Finished",
            Self::Abandoned => "Abandoned",
        }
    }
}

/// One claim that the work is done, and the evidence it rests on.
///
/// Built by [`Completion::claim`] and [`Completion::abandon`], and admitted only
/// by [`Completion::admit`]. Nothing here is an interpreter: `named` is a list of
/// opaque references the run already holds or does not, and the claim's `summary`
/// is text that is retained and never read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// What the claim asserts.
    kind: CompletionKind,
    /// The claim's own words, retained and never interpreted.
    summary: String,
    /// The evidence references the claim names.
    named: Vec<String>,
}

impl Completion {
    /// A claim that the work finished, resting on `named`.
    #[must_use]
    pub fn claim(summary: &str, named: &[&str]) -> Self {
        Self {
            kind: CompletionKind::Finished,
            summary: summary.to_owned(),
            named: named.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    /// A claim that the work is abandoned.
    ///
    /// Abandoning needs no evidence: a run may legitimately stop, and refusing
    /// to let it say so would force a finished-looking outcome on a run that
    /// produced nothing. What the kind makes observable is that the two are not
    /// reported as the same thing.
    #[must_use]
    pub fn abandon(summary: &str) -> Self {
        Self {
            kind: CompletionKind::Abandoned,
            summary: summary.to_owned(),
            named: Vec::new(),
        }
    }

    /// What the claim asserts.
    #[must_use]
    pub const fn kind(&self) -> CompletionKind {
        self.kind
    }

    /// The claim's own words.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// The evidence references the claim names.
    #[must_use]
    pub fn named(&self) -> &[String] {
        &self.named
    }

    /// Admit this claim against the evidence `present` names.
    ///
    /// `present` is what the run *actually holds* — the evidence references its
    /// artifact store can resolve, the receipts its journal committed. The claim
    /// is admitted only when every reference it names is in there, and the
    /// admitted coverage is then [`Coverage::Complete`], which is the only way
    /// that value is ever constructed.
    ///
    /// A claim naming more than [`MAX_EVIDENCE_REFS`] references is refused
    /// whole: trimming it would assert completeness over the prefix that was kept.
    ///
    /// # Errors
    ///
    /// [`Refusal::NotEvidenced`] naming the references and how many are absent,
    /// or [`Refusal::Limit`] when the claim named more references than the
    /// ceiling.
    pub fn admit(&self, present: &[String]) -> Result<Coverage, Refusal> {
        if self.named.len() > MAX_EVIDENCE_REFS {
            let refusal = Err(Refusal::Limit {
                what: "the number of evidence references a completion claim names",
                got: u64::saturating_from(self.named.len()),
                limit: u64::saturating_from(MAX_EVIDENCE_REFS),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "admit: returning an error to the caller");
            return refusal;
        }
        let missing = self
            .named
            .iter()
            .filter(|reference| !present.contains(reference))
            .count();
        if missing == 0 {
            Ok(Coverage::Complete)
        } else {
            Err(Refusal::NotEvidenced {
                named: self.named.clone(),
                missing,
            })
        }
    }
}

/// What admitting a completion claim produced.
///
/// Three distinct facts, and the type is what keeps them distinct: a caller that
/// wants to know "did it finish?" cannot get [`CompletionOutcome::NotEvidenced`]
/// to read as success, because success is a different variant rather than a field
/// inside a shared one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompletionOutcome {
    /// The claim is admitted and the coverage it establishes.
    ///
    /// Carries the claim's own kind, so an admitted abandonment is visibly not an
    /// admitted finish.
    Admitted {
        /// What the claim asserted.
        kind: CompletionKind,
        /// The coverage it established, which is complete.
        coverage: Coverage,
    },
    /// The claim named evidence the run does not hold.
    ///
    /// A refusal with the missing references named, and the provenance of the
    /// claim's bytes when the claim came from a payload.
    NotEvidenced {
        /// Why it was not admitted, naming the references.
        refusal: Box<Refusal>,
        /// The provenance of the claim, when it was decoded from bytes.
        provenance: Option<Provenance>,
    },
    /// The claim covered only part of what was asked for.
    ///
    /// Distinct from [`CompletionOutcome::NotEvidenced`] because the evidence was
    /// *present* and the coverage was still short: the run has the receipts and
    /// did not finish the work, which is a different report than a run that
    /// finished without receipts.
    Incomplete {
        /// The coverage the claim did establish.
        coverage: Coverage,
    },
    /// The run reached a typed intervention instead of repairing again.
    Intervention(Intervention),
}

impl CompletionOutcome {
    /// Whether the claim was admitted.
    #[must_use]
    pub const fn is_admitted(&self) -> bool {
        matches!(self, Self::Admitted { .. })
    }

    /// The refusal, when the claim named absent evidence.
    #[must_use]
    pub fn refusal(&self) -> Option<&Refusal> {
        match *self {
            // The binding is `&Box<Refusal>` under the implicit borrow, so the
            // coercion to `&Refusal` is written rather than left to inference.
            Self::NotEvidenced { ref refusal, .. } => Some(refusal.as_ref()),
            Self::Admitted { .. } | Self::Incomplete { .. } | Self::Intervention(_) => None,
        }
    }

    /// Admit `claim` against `present`, then apply `coverage`.
    ///
    /// The one place both checks run, so a caller cannot check the evidence and
    /// forget the coverage, or the other way round. `coverage` is what the run
    /// independently established; the claim's own words never enter it.
    ///
    /// An abandoned claim is admitted without evidence, because a run that stops
    /// is not making a completeness claim, and reporting it as `NotEvidenced`
    /// would name evidence the run was never going to gather.
    #[must_use]
    pub fn settle(claim: &Completion, present: &[String], coverage: Coverage) -> Self {
        if claim.kind() == CompletionKind::Abandoned {
            return Self::Admitted {
                kind: claim.kind(),
                coverage,
            };
        }
        match claim.admit(present) {
            Err(refusal) => Self::NotEvidenced {
                refusal: Box::new(refusal),
                provenance: None,
            },
            Ok(_) if !coverage.is_complete() => Self::Incomplete { coverage },
            Ok(complete) => Self::Admitted {
                kind: claim.kind(),
                coverage: complete,
            },
        }
    }

    /// Attach the provenance of the payload a claim was decoded from.
    #[must_use]
    pub fn from(&self, provenance: Provenance) -> Self {
        match *self {
            Self::NotEvidenced {
                ref refusal,
                provenance: _,
            } => Self::NotEvidenced {
                refusal: refusal.clone(),
                provenance: Some(provenance),
            },
            Self::Admitted {
                ref kind,
                ref coverage,
            } => Self::Admitted {
                kind: *kind,
                coverage: *coverage,
            },
            Self::Incomplete { ref coverage } => Self::Incomplete {
                coverage: *coverage,
            },
            Self::Intervention(ref intervention) => Self::Intervention(intervention.clone()),
        }
    }

    /// Build the outcome for a payload that was refused before it became a claim.
    #[must_use]
    pub fn from_outcome(outcome: &Outcome) -> Self {
        match *outcome {
            Outcome::Refused {
                ref refusal,
                ref provenance,
            } => Self::NotEvidenced {
                refusal: refusal.clone(),
                provenance: Some(provenance.clone()),
            },
            Outcome::Intervention(ref intervention) => Self::Intervention(intervention.clone()),
            Outcome::Admitted { .. } => Self::Incomplete {
                coverage: Coverage::Partial {
                    covered: 0,
                    asked: 1,
                },
            },
        }
    }
}

impl fmt::Display for CompletionOutcome {
    /// The arm and what it established.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Admitted { kind, coverage } => {
                write!(formatter, "admitted {} at {coverage}", kind.label())
            }
            Self::NotEvidenced {
                ref refusal,
                provenance: Some(ref provenance),
            } => write!(formatter, "not evidenced: {refusal} (from {provenance})"),
            Self::NotEvidenced {
                ref refusal,
                provenance: None,
            } => write!(formatter, "not evidenced: {refusal}"),
            Self::Incomplete { coverage } => write!(formatter, "incomplete: {coverage}"),
            Self::Intervention(ref intervention) => write!(formatter, "{intervention}"),
        }
    }
}
