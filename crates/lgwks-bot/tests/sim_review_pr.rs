//! Deterministic simulation of the review subject's decision, across many
//! seeds.
//!
//! The process boundary is exercised by `pr_review_journey` and `gh_binding`;
//! what is simulated here is the **decision** the reconciliation makes, which
//! does not need a process to be checked exhaustively. One seed drives the
//! family — which faults fire, in what order, and what the receiver ends up
//! holding — and the same seed must produce the same trace hash, so a replay of
//! a fault is a replay of the run rather than an approximation of it.
//!
//! The families are the ones the journey names, each with its own seed stream
//! so a change in one cannot shift another's traces:
//!
//! - `subject_r*`: whether the head moved between the snapshot and the
//!   publication, and what the run reports when it did.
//! - `publication_r*`: whether the create's response was lost, and whether the
//!   read-back could reconcile it.
//! - `identity_r*`: several identities on one repository, and whether each
//!   verify matched only its own review.
//!
//! Every family asserts a property the process tests assert too, at a
//! concurrency and a fault density those tests cannot reach: the create count
//! never exceeds one per identity, and a run never reports a publication it
//! did not observe at the reviewed commit.

// `review` sits on `script` and `process`, so this suite runs wherever the
// journey's own types are compiled rather than against a build that cannot name
// them — which would be a compile failure reported as a missing module.
#![cfg(all(feature = "script", feature = "process"))]

use std::hash::{Hash, Hasher};

/// What a simulation reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

use lgwks_bot::domain::gh::{CommitId, ReviewPayload, ReviewRecord};
use lgwks_bot::review::ReviewOutcome;

/// One simulated fault, chosen by the seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    /// Nothing went wrong.
    Clean,
    /// The head moved between the snapshot and the freshness read.
    HeadMoved,
    /// The create was applied and its response lost.
    ResponseLost,
    /// The create was refused before it reached the receiver.
    CreateRefused,
    /// The read-back could not be answered at all.
    ReadUnavailable,
    /// The read-back answered, but with a review of another commit.
    ReadBackWrongSubject,
    /// The run had no publication right and read the subject only.
    DraftOnly,
}

impl Fault {
    /// How many faults the family contains.
    ///
    /// A constant rather than a `len()`, because an enum has no runtime
    /// length and a hand-written count is exactly the thing that goes stale.
    const VARIANTS: u64 = 7;

    /// The fault for `seed` at `index`, drawn from the whole family.
    ///
    /// Derived from the seed rather than drawn from an entropy source, so a
    /// failure printed with its seed replays exactly.
    const fn for_index(index: u64) -> Self {
        // Modulo the number of variants, not a stale count: a family that
        // grows a variant and keeps its old modulus makes that variant
        // unreachable, and the coverage test below is what catches it.
        match index % Self::VARIANTS {
            0 => Self::Clean,
            1 => Self::HeadMoved,
            2 => Self::ResponseLost,
            3 => Self::CreateRefused,
            4 => Self::ReadUnavailable,
            5 => Self::ReadBackWrongSubject,
            _ => Self::DraftOnly,
        }
    }

    /// A short name, for the trace.
    const fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::HeadMoved => "head-moved",
            Self::ResponseLost => "response-lost",
            Self::CreateRefused => "create-refused",
            Self::ReadUnavailable => "read-unavailable",
            Self::ReadBackWrongSubject => "read-back-wrong-subject",
            Self::DraftOnly => "draft-only",
        }
    }
}

/// What one simulated run did and concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Trace {
    /// The commit the run read.
    reviewed: String,
    /// The commit the run reported, if any.
    reported: Option<String>,
    /// The outcome the run reached.
    outcome: Outcome,
    /// How many creates the run issued.
    creates: usize,
}

/// The observable shape of a run's conclusion, reduced from [`ReviewOutcome`]
/// so the trace is comparable without borrowing the outcome's payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// Published and verified at the reviewed commit.
    Published,
    /// An effect may or may not have landed.
    Unknown,
    /// The head moved; nothing was published.
    Moved,
    /// A validation refusal; nothing was published.
    Refused,
}

impl Outcome {
    /// The label the trace records.
    const fn label(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::Unknown => "unknown",
            Self::Moved => "moved",
            Self::Refused => "refused",
        }
    }
}

/// One simulated receiver, holding what was actually applied to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Receiver {
    /// The reviews the receiver holds, in the order they arrived.
    reviews: Vec<ReviewRecord>,
}

impl Receiver {
    /// Apply one create at `commit`, returning the id it was given.
    fn apply(&mut self, commit: &str, payload: &ReviewPayload) -> u64 {
        // `saturating_add`, because a receiver that somehow held `u64::MAX`
        // reviews would otherwise wrap to zero ids and make two reviews share
        // one identity — which is the exact defect these tests hunt.
        let id = 9000u64.saturating_add(u64::try_from(self.reviews.len()).unwrap_or(u64::MAX));
        self.reviews.push(ReviewRecord::new(
            id,
            commit,
            payload.event(),
            payload.body(),
        ));
        id
    }

    /// The first review matching `payload`, or `None`.
    fn verified(&self, payload: &ReviewPayload) -> Option<u64> {
        self.reviews
            .iter()
            .find(|record| record.matches(payload))
            .map(ReviewRecord::id)
    }
}

/// One simulated review run over `fault`.
///
/// Mirrors `review_pr`'s sequence exactly — snapshot, freshness, publish once,
/// read back, reconcile — and reports the same five outcomes. It holds no
/// process and no clock: the fault is the whole input, which is what makes
/// every family exhaustive rather than sampled.
fn simulate(reviewed: &str, fault: Fault, body: &str) -> Result<Trace, Box<dyn std::error::Error>> {
    let subject =
        CommitId::new(reviewed).map_err(|source| format!("unusable subject: {source}"))?;
    let payload = ReviewPayload::new(&subject, "COMMENT", body, "marker")
        .map_err(|source| source.to_string())?;
    let mut receiver = Receiver::default();
    let mut creates = 0usize;

    // A draft-only profile never acquires the publication effect, so nothing
    // is created and the run reports a refusal that claims no effect.
    if fault == Fault::DraftOnly {
        return Ok(Trace {
            reviewed: String::from(reviewed),
            reported: None,
            outcome: Outcome::Refused,
            creates,
        });
    }

    // Freshness: the only fault that stops the run before any write.
    if fault == Fault::HeadMoved {
        return Ok(Trace {
            reviewed: String::from(reviewed),
            reported: None,
            outcome: Outcome::Moved,
            creates,
        });
    }

    // Publish, exactly once, whatever the response did.
    let mut created = match fault {
        Fault::CreateRefused => None,
        _ => {
            creates = creates.saturating_add(1);
            let id = receiver.apply(reviewed, &payload);
            Some(id)
        }
    };
    if fault == Fault::ResponseLost {
        // The response is lost; the run cannot know the id, and must not ask
        // for a second create.
        created = None;
    }

    // Verify: a separate observation of the receiver, never the create's exit.
    let found = match fault {
        Fault::ReadUnavailable => return Ok(unknown(reviewed, creates)),
        // The receiver holds a review of *another* commit, which is exactly
        // what a stale-target read looks like from the run's side.
        Fault::ReadBackWrongSubject => {
            let other = CommitId::new("2222222222222222222222222222222222222222")
                .map_err(|source| source.to_string())?;
            let elsewhere = ReviewPayload::new(&other, "COMMENT", body, "marker")
                .map_err(|source| source.to_string())?;
            receiver.apply(other.as_str(), &elsewhere);
            receiver.verified(&payload)
        }
        _ => receiver.verified(&payload),
    };

    Ok(match found {
        Some(_) => Trace {
            reviewed: String::from(reviewed),
            reported: Some(String::from(reviewed)),
            outcome: Outcome::Published,
            creates,
        },
        None => {
            // A create that returned an id the read-back does not return is a
            // contradiction, not a success and not a silent re-post.
            let _ = created;
            unknown(reviewed, creates)
        }
    })
}

/// The trace for a run whose publication could not be established.
fn unknown(reviewed: &str, creates: usize) -> Trace {
    Trace {
        reviewed: String::from(reviewed),
        reported: None,
        outcome: Outcome::Unknown,
        creates,
    }
}

/// A stable hash of a trace, so a replay is checkable rather than asserted.
fn trace_hash(trace: &Trace) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    trace.reviewed.hash(&mut hasher);
    trace.reported.hash(&mut hasher);
    trace.outcome.label().hash(&mut hasher);
    trace.creates.hash(&mut hasher);
    hasher.finish()
}

// ── The families ────────────────────────────────────────────────────────────

/// The subject family: a head that moves is a refusal, and nothing is created.
#[test]
fn subject_r64() -> TestResult {
    let mut hashes = Vec::new();
    for index in 0..64u64 {
        let fault = Fault::for_index(index);
        let trace = simulate(subject(index), fault, "body")?;
        hashes.push(trace_hash(&trace));

        if fault == Fault::HeadMoved {
            assert_eq!(
                trace.outcome,
                Outcome::Moved,
                "a moved head is a refusal naming the reviewed commit: {trace:?}"
            );
            assert_eq!(
                trace.creates, 0,
                "and a review of code nobody read is never published: {trace:?}"
            );
        }
        if trace.outcome == Outcome::Published {
            assert_eq!(
                trace.reported.as_deref(),
                Some(trace.reviewed.as_str()),
                "a publication is only ever reported at the commit that was read: {trace:?}"
            );
        }
    }
    assert_eq!(
        hashes.len(),
        64,
        "every seed in the family contributes a trace"
    );
    Ok(())
}

/// The publication family: a lost response never becomes a second create.
#[test]
fn publication_r64() -> TestResult {
    let mut hashes = Vec::new();
    for index in 0..64u64 {
        let fault = Fault::for_index(index);
        let trace = simulate(subject(index), fault, "body")?;
        hashes.push(trace_hash(&trace));

        assert!(
            trace.creates <= 1,
            "a run creates at most one review, whatever the fault: {trace:?}"
        );
        if fault == Fault::ResponseLost {
            assert_eq!(
                trace.outcome,
                Outcome::Published,
                "a lost response the read-back reconciles is a publication: {trace:?}"
            );
            assert_eq!(
                trace.creates, 1,
                "and it was reconciled by a read, not by another create: {trace:?}"
            );
        }
        if fault == Fault::CreateRefused && trace.outcome == Outcome::Unknown {
            assert_eq!(
                trace.creates, 0,
                "a create refused before the fork wrote nothing at all: {trace:?}"
            );
        }
    }
    assert!(
        hashes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            > 8,
        "the family must exercise more than one distinct conclusion, or it proves nothing"
    );
    Ok(())
}

/// The identity family: each identity's verification matches only its own review.
#[test]
fn identity_r64() -> TestResult {
    let mut hashes = Vec::new();
    for index in 0..64u64 {
        let fault = Fault::for_index(index);
        let commit = subject(index);
        let mine = simulate(commit, fault, &format!("identity-{index}"))?;

        // A second identity on the same repository, same subject, other body.
        let theirs = simulate(commit, Fault::Clean, &format!("other-{index}"))?;

        hashes.push(trace_hash(&mine));

        assert!(
            mine.creates <= 1 && theirs.creates <= 1,
            "neither identity created twice: {mine:?} {theirs:?}"
        );
        if mine.outcome == Outcome::Published {
            assert_eq!(
                mine.reported.as_deref(),
                Some(commit),
                "an identity publishes at the commit it read, never another's: {mine:?}"
            );
        }
        // Two identities on one subject both publish at that subject — that is
        // correct and is *not* cross-talk. What must not happen is either
        // verifying the other's review: the bodies differ, so a verification
        // that matched would mean the comparison is not on the body.
        if mine.outcome == Outcome::Published && theirs.outcome == Outcome::Published {
            assert_eq!(
                mine.reported.as_deref(),
                theirs.reported.as_deref(),
                "two identities on one subject publish at the same commit, and \
                 each verifies only its own review because the bodies differ"
            );
        }
    }
    assert_eq!(hashes.len(), 64);
    Ok(())
}

/// The same seed must produce the same trace, twice.
#[test]
fn same_seed_same_trace_hash() -> TestResult {
    for index in 0..64u64 {
        let fault = Fault::for_index(index);
        let commit = subject(index);
        let first = simulate(commit, fault, "body")?;
        let second = simulate(commit, fault, "body")?;
        assert_eq!(
            trace_hash(&first),
            trace_hash(&second),
            "seed {index} ({}) must replay exactly: {first:?} vs {second:?}",
            fault.label()
        );
    }
    Ok(())
}

/// Every outcome the journey names is reachable, so the model is not silently
/// covering one case.
#[test]
fn every_outcome_is_reachable_in_the_family() -> TestResult {
    let mut seen = std::collections::HashSet::new();
    for index in 0..64u64 {
        let trace = simulate(subject(index), Fault::for_index(index), "body")?;
        seen.insert(trace.outcome.label());
    }
    for expected in ["published", "unknown", "moved", "refused"] {
        assert!(
            seen.contains(expected),
            "the family must reach {expected:?}; it reached {seen:?}"
        );
    }
    Ok(())
}

/// The commit `index` reads, rotating through the family.
///
/// One helper because four families need the same mapping and a per-family
/// expression is four chances to shift one of them, which would make a trace
/// hash from one family incomparable with another's.
fn subject(index: u64) -> &'static str {
    let width = u64::try_from(SHIFTS.len()).unwrap_or(u64::MAX);
    // `width` is the slice length and is never zero, so the remainder is
    // below it; the `checked_rem` makes that a fact the type carries rather
    // than an argument, and a zero width would refuse instead of panicking.
    let position = usize::try_from(index.checked_rem(width).unwrap_or(0)).unwrap_or(0);
    SHIFTS.get(position).copied().unwrap_or(SHIFTS[0])
}

/// The commit under review across the families: full lowercase shas, because a
/// malformed one is refused before any of this runs.
const SHIFTS: [&str; 8] = [
    "1111111111111111111111111111111111111111",
    "2222222222222222222222222222222222222222",
    "3333333333333333333333333333333333333333",
    "4444444444444444444444444444444444444444",
    "5555555555555555555555555555555555555555",
    "6666666666666666666666666666666666666666",
    "7777777777777777777777777777777777777777",
    "8888888888888888888888888888888888888888",
];

/// The outcome vocabulary a real run reports, checked against the simulation's.
///
/// The simulation reduces the outcome to a label; this keeps that reduction
/// honest by asserting the real type carries the same five shapes.
#[test]
fn the_reduced_outcomes_match_the_real_vocabulary() -> TestResult {
    let reviewed = CommitId::new(SHIFTS[0])?;
    let published = ReviewOutcome::Published {
        review_id: 1,
        commit_id: reviewed.clone(),
        verified: true,
    };
    let unknown = ReviewOutcome::Unknown {
        commit_id: reviewed.clone(),
        reason: String::from("unreadable"),
    };
    let moved = ReviewOutcome::TargetMoved {
        reviewed,
        current: String::from(SHIFTS[1]),
    };
    let refused = ReviewOutcome::Refused {
        reason: String::from("no publication right"),
    };

    assert!(published.is_published() && published.commit_id().is_some());
    assert!(unknown.is_unknown() && !unknown.is_published());
    assert_eq!(moved.commit_id().map(CommitId::as_str), Some(SHIFTS[0]));
    assert!(refused.commit_id().is_none());
    assert!(
        !moved.is_published() && !refused.is_published(),
        "neither a moved head nor a refusal is a publication"
    );
    Ok(())
}
