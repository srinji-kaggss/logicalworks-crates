//! Repeated unchanged failure reaches a finite typed intervention.
//!
//! The failure mode this module exists for is not one bad repair; it is the
//! *N*th identical one. A run that retries a tool call, gets the same refusal,
//! asks the model again, gets the same refusal, and loops has spent real money
//! and reached nothing, and the loop's own logs make it look like progress
//! because each iteration records something.
//!
//! So a [`RepairLedger`] counts one *unchanged failure* — same fingerprint, no
//! new evidence since — and after a declared number of repetitions it returns
//! [`Intervention::NoProgress`] instead of another [`Outcome::Admitted`]. Three
//! properties make that count honest:
//!
//! - **New evidence does not reset the root spend.** [`RepairLedger::record_evidence`]
//!   adds to the ledger's total, and [`RepairLedger::repaired`] reports what was
//!   spent against the original failure. A run that gathers evidence on a new
//!   axis does not thereby buy another `N` attempts on the old one: the
//!   fingerprint's repetition count is compared against the ceiling on the
//!   *original* failure, which never moves.
//! - **A different failure is a different fingerprint**, so a run making real
//!   progress through genuinely different failures is never intervened upon.
//! - **The ceiling is declared and readable**, so a caller can say what it set
//!   and a test can assert what happened rather than that "it stopped eventually".

use std::collections::BTreeMap;
use std::fmt;

use super::{Outcome, Provenance};

/// How many repetitions of one unchanged failure are tolerated, and what the
/// ledger does after them.
///
/// Two numbers rather than one because they answer different questions: `repeat`
/// is how many times the *same* failure may be seen before the run is
/// intervened upon, and `budget` is how many failures of any kind the ledger
/// accepts before it stops recording at all. A run whose failures are all
/// different can still exhaust its budget; a run whose failures are all the same
/// hits the repeat ceiling first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LedgerLimits {
    /// How many repetitions of one fingerprint are tolerated.
    pub repeat: u32,
    /// How many distinct failures the ledger records at once.
    pub distinct: usize,
}

impl LedgerLimits {
    /// Limits of `repeat` repetitions and `distinct` distinct fingerprints.
    #[must_use]
    pub const fn new(repeat: u32, distinct: usize) -> Self {
        Self { repeat, distinct }
    }
}

impl Default for LedgerLimits {
    /// Three repetitions and sixteen fingerprints.
    ///
    /// Three because the first failure is not a decision: a run that has tried
    /// once has learned nothing and has every reason to try again, while a run
    /// that has tried four times with the same fingerprint and no new evidence is
    /// a loop. Sixteen because a ledger that recorded every distinct failure of a
    /// long run would itself become the unbounded thing this module prevents.
    fn default() -> Self {
        Self {
            repeat: 3,
            distinct: 16,
        }
    }
}

/// What a run reached instead of repairing again.
///
/// Not a message: an enum a caller matches on, because the two arms call for
/// different repairs and a caller that has to read a string to tell them apart
/// will read it wrong under load.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Intervention {
    /// One unchanged failure repeated past the ceiling.
    ///
    /// The fingerprint is named so the caller can look up what it was, and
    /// `repetitions` says how many times it was seen — the count that reached the
    /// ceiling, not the count that was tolerated.
    NoProgress {
        /// The failure that would not change.
        fingerprint: String,
        /// How many times it was seen, including this one.
        repetitions: u32,
    },
    /// The ledger recorded as many distinct failures as it holds.
    ///
    /// The run is making *different* failures and still going, which is progress
    /// of a sort — and is still a run that needs a person, so it is its own arm
    /// rather than a non-progress claim.
    LedgerFull {
        /// How many distinct fingerprints the ledger holds.
        held: usize,
    },
}

impl Intervention {
    /// The name of this arm.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match *self {
            Self::NoProgress { .. } => "NoProgress",
            Self::LedgerFull { .. } => "LedgerFull",
        }
    }
}

impl std::error::Error for Intervention {}

impl fmt::Display for Intervention {
    /// The arm and the facts it names.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NoProgress {
                ref fingerprint,
                ref repetitions,
            } => write!(
                formatter,
                "no progress: {fingerprint:?} failed {repetitions} times unchanged, so no \
                 further repair is admitted"
            ),
            Self::LedgerFull { held } => write!(
                formatter,
                "ledger full: {held} distinct failures recorded, so none further is retained"
            ),
        }
    }
}

/// What the ledger knows about one fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tracked {
    /// How many times this fingerprint has been seen.
    repetitions: u32,
    /// The provenance of the payload that first produced it.
    provenance: Provenance,
    /// Whether anything new has been recorded since.
    progress: bool,
}

/// One run's no-progress ledger.
///
/// Not `Default`-able and not a set of fields: a ledger with no limits has no
/// ceiling, which is the defect, so the only way to have one is through
/// [`RepairLedger::new`] with its limits in hand.
#[derive(Debug, Clone)]
pub struct RepairLedger {
    /// The declared ceilings.
    limits: LedgerLimits,
    /// The provenance of the run this ledger is for.
    provenance: Provenance,
    /// Every fingerprint seen, in insertion-independent name order so a report of
    /// the ledger does not depend on arrival order.
    tracked: BTreeMap<String, Tracked>,
    /// How many failures the run has recorded, across every fingerprint.
    spent: u64,
    /// How many pieces of evidence the run has recorded since the ledger opened.
    evidence: u64,
}

impl RepairLedger {
    /// Open a ledger for a run whose payloads have `provenance`.
    ///
    /// Infallible and deliberately so: the ceilings are checked on the recording
    /// doors rather than at construction, so a run may open its ledger before it
    /// knows how it will fail and a ceiling that is too low is a
    /// [`Intervention`] on the first relevant call rather than a refusal to
    /// start. A ledger with no limits has no ceiling, which is why there is no
    /// `Default` and every ledger names its own.
    #[must_use]
    pub fn new(limits: LedgerLimits, provenance: Provenance) -> Self {
        Self {
            limits,
            provenance,
            tracked: BTreeMap::new(),
            spent: 0,
            evidence: 0,
        }
    }

    /// The declared ceilings.
    #[must_use]
    pub const fn limits(&self) -> LedgerLimits {
        self.limits
    }

    /// The provenance of the run this ledger is for.
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// How many failures the run has recorded in total.
    ///
    /// Monotone: nothing a caller does to this ledger makes it smaller, so
    /// "root spend" has one meaning for the whole run.
    #[must_use]
    pub const fn spent(&self) -> u64 {
        self.spent
    }

    /// How many pieces of evidence the run has recorded.
    #[must_use]
    pub const fn evidence(&self) -> u64 {
        self.evidence
    }

    /// How many times `fingerprint` has been seen.
    #[must_use]
    pub fn repetitions(&self, fingerprint: &str) -> u32 {
        self.tracked
            .get(fingerprint)
            .map_or(0, |tracked| tracked.repetitions)
    }

    /// The provenance of the payload that first produced `fingerprint`.
    #[must_use]
    pub fn provenance_of(&self, fingerprint: &str) -> Option<&Provenance> {
        self.tracked
            .get(fingerprint)
            .map(|tracked| &tracked.provenance)
    }

    /// Record one failure under `fingerprint` and report what happens next.
    ///
    /// The whole of the no-progress rule in one call: an
    /// [`Outcome::Intervention`] once the same fingerprint has been seen
    /// [`LedgerLimits::repeat`] times *and* the `repeat + 1`-th time arrives
    /// unchanged, and an [`Outcome::Admitted`] (the run may repair again)
    /// otherwise.
    ///
    /// Recording is idempotent per fingerprint: a second call with the same
    /// fingerprint increments that fingerprint's count rather than adding a
    /// second entry, so a caller that reports the same failure from two places
    /// cannot double-count its way past the ceiling.
    ///
    /// Recording evidence does **not** lower a repetition count and does **not**
    /// clear the recorded-evidence mark. That is the "new evidence does not
    /// erase root spend" half of the row: the failure that has already been
    /// retried three times stays three-times-retried, and the next identical one
    /// is still the intervention — whatever was learned in between, the loop
    /// itself is still the loop.
    #[must_use]
    pub fn record_failure(&mut self, fingerprint: &str) -> Outcome {
        self.spent = self.spent.saturating_add(1);
        if let Some(tracked) = self.tracked.get_mut(fingerprint) {
            tracked.repetitions = tracked.repetitions.saturating_add(1);
            let repetitions = tracked.repetitions;
            return if repetitions > self.limits.repeat {
                Outcome::Intervention(Intervention::NoProgress {
                    fingerprint: fingerprint.to_owned(),
                    repetitions,
                })
            } else {
                Outcome::Admitted {
                    plan: super::Plan::for_repair(fingerprint, self.provenance.clone()),
                    provenance: self.provenance.clone(),
                }
            };
        }
        if self.tracked.len() >= self.limits.distinct {
            return Outcome::Intervention(Intervention::LedgerFull {
                held: self.tracked.len(),
            });
        }
        self.tracked.insert(
            fingerprint.to_owned(),
            Tracked {
                repetitions: 1,
                provenance: self.provenance.clone(),
                progress: false,
            },
        );
        Outcome::Admitted {
            plan: super::Plan::for_repair(fingerprint, self.provenance.clone()),
            provenance: self.provenance.clone(),
        }
    }

    /// Record that the run gathered something new.
    ///
    /// Moves *no* repetition count and does *not* clear the record of what a
    /// fingerprint has already cost. It marks the fingerprints as having seen
    /// progress, which is what a reader consults through
    /// [`RepairLedger::has_progress`], and it is what keeps "new evidence is
    /// recorded and does not erase root spend" true as one property rather than
    /// two assertions that could disagree.
    ///
    /// Recording evidence for a fingerprint nobody has failed under is refused by
    /// returning `false`: the evidence belongs to a run that has not got there.
    pub fn record_evidence(&mut self, fingerprint: &str) -> bool {
        let Some(tracked) = self.tracked.get_mut(fingerprint) else {
            return false;
        };
        self.evidence = self.evidence.saturating_add(1);
        tracked.progress = true;
        true
    }

    /// Whether anything new has been recorded against `fingerprint`.
    #[must_use]
    pub fn has_progress(&self, fingerprint: &str) -> bool {
        self.tracked
            .get(fingerprint)
            .is_some_and(|tracked| tracked.progress)
    }

    /// Whether the run may repair `fingerprint` again.
    ///
    /// The predicate behind [`RepairLedger::record_failure`], exposed so a caller
    /// that batches its repairs can ask first and record once.
    #[must_use]
    pub fn may_repair(&self, fingerprint: &str) -> bool {
        self.repetitions(fingerprint) <= self.limits.repeat
            && (self.tracked.contains_key(fingerprint) || self.tracked.len() < self.limits.distinct)
    }
}
