//! What a run must not lose when its context is reset.
//!
//! A context reset is not a restart. The run keeps its identity, its store and
//! its record count; what goes is the working memory — the model's view of where
//! it got to. The question this module answers is: what has to cross that gap
//! for the new instance to be *the same run* rather than a fresh one that
//! happens to share a run id?
//!
//! Four things, and each one is a thing a naive checkpoint loses:
//!
//! | Carried | Why it is load-bearing |
//! |---|---|
//! | [`Checkpoint::steps`] | work already finished; re-running it is at best waste and at worst a second effect |
//! | [`Checkpoint::corrections`] | what the *user* said is not a fact the model can re-derive, and losing it is how a run undoes a correction it was already given |
//! | [`Checkpoint::effects`] | an effect whose outcome is `Unknown` must stay `Unknown` across the reset; a checkpoint that dropped it would let the new instance treat it as never attempted |
//! | [`Checkpoint::evidence`] | the references a claim rests on; without them a completion claim re-derives from nothing and fails the evidence check for the wrong reason |
//!
//! # It round-trips through the run store
//!
//! [`Checkpoint`] is `Durable`, so `remember` archives it against the run id and
//! a *new* task instance resuming the same run recovers it without the original
//! instance existing any more. That is the claim T27 makes, and it is the reason
//! this type is archivable rather than merely in memory: a checkpoint held only
//! in the instance that wrote it does not survive the reset it exists for.

use std::fmt;

use lgwks_std::wire::{Archive, Deserialize, Serialize};

use super::MAX_EVIDENCE_REFS;

/// The most completed steps one checkpoint carries.
pub const MAX_CHECKPOINT_STEPS: usize = 256;

/// The most user corrections one checkpoint carries.
pub const MAX_CHECKPOINT_NOTES: usize = 64;

/// The most evidence references one checkpoint carries.
pub const MAX_CHECKPOINT_EVIDENCE: usize = MAX_EVIDENCE_REFS;

/// What kind of correction the user gave.
///
/// Carried as a kind rather than as text because the two arms are used
/// differently: an *override* replaces what the run was about to do, and a
/// *refusal* forbids it. A checkpoint that stored both as strings would lose the
/// distinction across a reset, and the new instance would re-derive "do it
/// anyway" from a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Archive, Serialize, Deserialize)]
#[rkyv(crate = lgwks_std::wire::rkyv)]
#[non_exhaustive]
pub enum CorrectionKind {
    /// Do this instead of what you planned.
    Override,
    /// Do not do this at all.
    Refusal,
}

impl CorrectionKind {
    /// The name of this arm, for a report.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Override => "Override",
            Self::Refusal => "Refusal",
        }
    }
}

impl fmt::Display for CorrectionKind {
    /// The [`CorrectionKind::label`].
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// One thing the user said that the run must keep.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(crate = lgwks_std::wire::rkyv)]
pub struct Correction {
    /// Whether this replaces a plan or forbids one.
    kind: CorrectionKind,
    /// What the user said, verbatim.
    text: String,
}

impl Correction {
    /// A correction of `kind`, carrying `text` verbatim.
    #[must_use]
    pub fn new(kind: CorrectionKind, text: &str) -> Self {
        Self {
            kind,
            text: text.to_owned(),
        }
    }

    /// Whether this replaces a plan or forbids one.
    #[must_use]
    pub const fn kind(&self) -> CorrectionKind {
        self.kind
    }

    /// What the user said, verbatim.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// What is known about one external effect the run attempted.
///
/// The `Unknown` arm is the load-bearing one and is why this is an enum rather
/// than a list of applied effects: an effect whose outcome is unknown must
/// survive a context reset *as unknown*, because the alternative is a new
/// instance that either re-sends it (a second effect) or assumes it failed (an
/// unaccounted-for one). Both are worse than saying "I do not know".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Archive, Serialize, Deserialize)]
#[rkyv(crate = lgwks_std::wire::rkyv)]
#[non_exhaustive]
pub enum EffectNoteKind {
    /// The effect definitely did not happen.
    NotApplied,
    /// The effect definitely happened.
    Applied,
    /// The effect may or may not have happened, and no further attempt may be
    /// made without reconciling it.
    Unknown,
}

impl EffectNoteKind {
    /// The name of this arm.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NotApplied => "NotApplied",
            Self::Applied => "Applied",
            Self::Unknown => "Unknown",
        }
    }

    /// Whether this arm leaves the effect's outcome undecided.
    #[must_use]
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

impl fmt::Display for EffectNoteKind {
    /// The [`EffectNoteKind::label`].
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// What is known about one external effect, under the reference that named it.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(crate = lgwks_std::wire::rkyv)]
pub struct EffectNote {
    /// The effect identity this note is about.
    reference: String,
    /// What is known about it.
    kind: EffectNoteKind,
}

impl EffectNote {
    /// A note that `reference` is `kind`.
    #[must_use]
    pub fn new(reference: &str, kind: EffectNoteKind) -> Self {
        Self {
            reference: reference.to_owned(),
            kind,
        }
    }

    /// The effect identity this note is about.
    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// What is known about it.
    #[must_use]
    pub const fn kind(&self) -> EffectNoteKind {
        self.kind
    }
}

/// Everything one run must carry across a context reset.
///
/// Archivable through [`crate::script::remember`], so a new task instance
/// resuming the run reads it back from the run store rather than from the
/// instance that wrote it. Every list is bounded and every bound is a declared
/// constant; an append past a bound is a [`CheckpointError`] rather than a
/// silent drop, because a checkpoint that quietly forgot a correction is the
/// failure this type is here to prevent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(crate = lgwks_std::wire::rkyv)]
pub struct Checkpoint {
    /// Steps that finished, by step path.
    steps: Vec<String>,
    /// What the user said.
    corrections: Vec<Correction>,
    /// What is known about each attempted effect.
    effects: Vec<EffectNote>,
    /// The evidence references a claim may rest on.
    evidence: Vec<String>,
}

impl Checkpoint {
    /// The empty checkpoint, for the start of a run.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The steps that finished, in the order they were recorded.
    #[must_use]
    pub fn steps(&self) -> &[String] {
        &self.steps
    }

    /// What the user said, in the order they said it.
    #[must_use]
    pub fn corrections(&self) -> &[Correction] {
        &self.corrections
    }

    /// What is known about each attempted effect.
    #[must_use]
    pub fn effects(&self) -> &[EffectNote] {
        &self.effects
    }

    /// The evidence references a claim may rest on.
    #[must_use]
    pub fn evidence(&self) -> &[String] {
        &self.evidence
    }

    /// Whether `step` is already recorded as finished.
    #[must_use]
    pub fn completed(&self, step: &str) -> bool {
        self.steps.iter().any(|path| path == step)
    }

    /// How many effects are still undecided.
    #[must_use]
    pub fn unknowns(&self) -> usize {
        self.effects
            .iter()
            .filter(|note| note.kind().is_unknown())
            .count()
    }

    /// The evidence references present, as the completion check reads them.
    ///
    /// A borrow rather than a copy: the completion claim's evidence check
    /// compares against these, and handing it a clone would let a caller check a
    /// claim against a list that had drifted from the checkpoint.
    #[must_use]
    pub fn present(&self) -> &[String] {
        &self.evidence
    }

    /// Record a finished step.
    ///
    /// Idempotent: recording the same step twice is a no-op rather than a second
    /// entry, so a resumed step that re-announces itself does not inflate the
    /// count or trip the ceiling.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Limit`] naming the ceiling when the run has
    /// [`MAX_CHECKPOINT_STEPS`] distinct finished steps.
    pub fn complete(&mut self, step: &str) -> Result<(), CheckpointError> {
        if self.completed(step) {
            return Ok(());
        }
        check_push(&self.steps, MAX_CHECKPOINT_STEPS, "completed steps")?;
        self.steps.push(step.to_owned());
        Ok(())
    }

    /// Record what the user said.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Limit`] naming the ceiling when the run has
    /// [`MAX_CHECKPOINT_NOTES`] corrections.
    pub fn correct(&mut self, kind: CorrectionKind, text: &str) -> Result<(), CheckpointError> {
        self.corrections.push(Correction::new(kind, text));
        check_push(&self.corrections, MAX_CHECKPOINT_NOTES, "user corrections")
    }

    /// Record what is known about one effect.
    ///
    /// A later note about the same reference *replaces* the earlier one rather
    /// than joining it: reconciliation is how an `Unknown` becomes `Applied`, and
    /// a checkpoint holding both would leave the effect undecided in the only
    /// place that matters.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Limit`] naming the ceiling when the run has
    /// [`MAX_CHECKPOINT_NOTES`] distinct effect references.
    pub fn observe_effect(
        &mut self,
        reference: &str,
        kind: EffectNoteKind,
    ) -> Result<(), CheckpointError> {
        let note = EffectNote::new(reference, kind);
        match self
            .effects
            .iter_mut()
            .find(|held| held.reference() == reference)
        {
            // A *replacement* is never a growth, so it is never refused: the
            // ceiling bounds how many distinct references the checkpoint carries,
            // and reconciling one of them is exactly what it is for.
            Some(held) => {
                *held = note;
                Ok(())
            }
            // The charge comes before the append, so a refused effect note
            // leaves the checkpoint exactly as it was rather than one longer
            // than the caller was told.
            None => {
                check_push(&self.effects, MAX_CHECKPOINT_NOTES, "effect notes")?;
                self.effects.push(note);
                Ok(())
            }
        }
    }

    /// Record an evidence reference a completion claim may rest on.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Limit`] naming the ceiling when the run has
    /// [`MAX_CHECKPOINT_EVIDENCE`] references.
    pub fn record_evidence(&mut self, reference: &str) -> Result<(), CheckpointError> {
        if self.evidence.iter().any(|held| held == reference) {
            return Ok(());
        }
        check_push(
            &self.evidence,
            MAX_CHECKPOINT_EVIDENCE,
            "evidence references",
        )?;
        self.evidence.push(reference.to_owned());
        Ok(())
    }
}

/// Charge one append against `ceiling`, before the caller performs it.
///
/// Generic over the element so one rule covers all four lists. It reads the list
/// through a slice and mutates nothing, because the *caller* performs the append:
/// a rule that ran after the append would report a bound the list had already
/// broken, which is the defect this shape exists to rule out.
fn check_push<T>(list: &[T], ceiling: usize, what: &'static str) -> Result<(), CheckpointError> {
    let would_hold = list.len().saturating_add(1);
    if would_hold > ceiling {
        let refusal = Err(CheckpointError::Limit {
            what,
            got: u64::try_from(would_hold).unwrap_or(u64::MAX),
            limit: u64::try_from(ceiling).unwrap_or(u64::MAX),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "check_push: returning an error to the caller");
        return refusal;
    }
    Ok(())
}

// ── CheckpointError ──────────────────────────────────────────────────────────

/// Why a checkpoint could not record something.
///
/// A refusal, never a silent drop: the whole reason this type exists is that
/// what it forgets is what a reset would forget, and a caller that is told
/// nothing cannot tell a full checkpoint from a lossy one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckpointError {
    /// A declared ceiling refused the append, and nothing was recorded.
    Limit {
        /// Which list.
        what: &'static str,
        /// How many entries it now holds.
        got: u64,
        /// The ceiling.
        limit: u64,
    },
    /// The bytes are not this type's archive, or are not a complete one.
    ///
    /// A truncated archive is refused rather than decoded into a partial
    /// checkpoint, which is the row's "truncated data never becomes a
    /// full-coverage claim" applied to the checkpoint itself.
    Archive,
}

impl fmt::Display for CheckpointError {
    /// What was refused.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Limit { what, got, limit } => {
                write!(formatter, "Limit: {what} would hold {got}, past {limit}")
            }
            Self::Archive => {
                formatter.write_str("Archive: the bytes are not a complete checkpoint archive")
            }
        }
    }
}

impl std::error::Error for CheckpointError {}

impl From<CheckpointError> for super::Refusal {
    /// A checkpoint refusal is a proposal refusal, so one call site's `?` reads
    /// the same whether the ceiling was hit while decoding or while recording.
    fn from(error: CheckpointError) -> Self {
        match error {
            CheckpointError::Limit { what, got, limit } => Self::Limit { what, got, limit },
            CheckpointError::Archive => Self::Malformed {
                cause: "a checkpoint archive that is not a complete one",
                at: 0,
            },
        }
    }
}

impl From<CheckpointError> for crate::script::FlowError {
    /// A checkpoint refusal inside a task body.
    ///
    /// Permanent, because both arms are: a ceiling that was reached will still
    /// be reached, and an archive that will not decode will not decode on a
    /// retry. The text carries which, so the step's own location names the fact
    /// rather than the caller having to match on the variant.
    fn from(error: CheckpointError) -> Self {
        Self::failed(error)
    }
}

impl Checkpoint {
    /// Archive this checkpoint, the shape a run store frames it in.
    ///
    /// # Errors
    ///
    /// [`lgwks_std::wire::WireError`] when the value does not archive.
    pub fn to_record(&self) -> Result<Vec<u8>, lgwks_std::wire::WireError> {
        crate::script::run_store::Durable::to_record(self)
    }

    /// Rebuild a checkpoint from the bytes a run store framed.
    ///
    /// # Errors
    ///
    /// [`lgwks_std::wire::WireError`] when the bytes are not this type's
    /// archive — which includes a truncated one, because the checked decoder
    /// refuses an archive that does not cover its whole length.
    pub fn from_record(bytes: &[u8]) -> Result<Self, lgwks_std::wire::WireError> {
        crate::script::run_store::Durable::from_record(bytes)
    }
}
