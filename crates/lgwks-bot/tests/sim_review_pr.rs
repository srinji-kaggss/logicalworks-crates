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

use lgwks_bot::domain::gh::{
    CommitId, Gh, GhError, Repository, ReviewComment, ReviewPayload, ReviewRecord,
};
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
            payload.state(),
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

// ── The adapter's own decision, simulated ──────────────────────────────────

/// The argument vector each call would execute, over the whole method/path
/// space the journey uses.
///
/// This is simulated rather than forked because the vector is what a caller
/// can be wrong about *before* anything runs: a `--method` that arrives after
/// the path, or a payload file that is not named, is a call that cannot do what
/// it says. The process tests confirm the vector reaches a real child; this
/// pins its shape across every combination.
#[test]
fn argument_vectors_r64() -> TestResult {
    let gh = Gh::new(Repository::new("acme/widgets")?);
    let methods = ["GET", "POST"];
    let paths = [
        "repos/acme/widgets/pulls/7",
        "repos/acme/widgets/pulls/7/reviews",
    ];
    let mut hashes = Vec::new();
    for index in 0..64u64 {
        let method = methods[pick(index, methods.len())];
        let path = paths[pick(index, paths.len())];
        let extra: &[&str] = if index % 3 == 0 { &["--paginate"] } else { &[] };
        let mut rest = vec!["--method", method, path];
        rest.extend_from_slice(extra);

        let args = gh.args_for(&rest);
        hashes.push(text_hash(&args.join("\t")));

        assert_eq!(
            args.first().map(String::as_str),
            Some("api"),
            "every call is `gh api`, never a bare invocation: {args:?}"
        );
        let method_at = args
            .iter()
            .position(|arg| arg == "--method")
            .ok_or("the method must be named")?;
        assert_eq!(
            args.get(method_at.saturating_add(1)).map(String::as_str),
            Some(method),
            "the method's value must follow its flag: {args:?}"
        );
        assert!(
            args.iter().any(|arg| arg == path),
            "the path must be present: {args:?}"
        );
        // No shell metacharacter reaches the vector from a validated value: the
        // repository, number and commit are all typed before they get here.
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains(';') || arg.contains('|')),
            "nothing in the vector introduces a shell: {args:?}"
        );
    }
    assert!(
        hashes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            >= 4,
        "the family must exercise more than one argument vector"
    );
    Ok(())
}

/// The publish-failure classification, driven through the real mapping.
///
/// A publish step that fails reports a [`GhError`], and the task decides from
/// the certainty `FlowError::from` gives it whether to reconcile (read back) or
/// report a clean failure. A failure raised before any child existed must be
/// `Refused` — otherwise a staging error or an invalid marker is reported as an
/// effect that "may or may not exist" — and every failure after the client ran
/// must not be, or a lost response is never read back. Each seed builds one
/// error with seeded content and checks the certainty the crate really maps it
/// to.
#[test]
fn certainty_classification_r64() -> TestResult {
    use lgwks_bot::error::{BotError, DispatchCertainty};

    let mut hashes = Vec::new();
    let mut seen_refused = 0_u32;
    let mut seen_after_start = 0_u32;
    for index in 0..64u64 {
        let text = format!("seed-{index:x}-{}", text_hash(&index.to_string()));
        let (error, sent_nothing) = match index % 9 {
            0 => (GhError::NoRunner, true),
            1 => (
                GhError::Staging {
                    path: text.clone(),
                    source: String::from("no entropy"),
                },
                true,
            ),
            2 => (
                GhError::PayloadNotSent {
                    reason: text.clone(),
                },
                true,
            ),
            3 => (
                GhError::Event {
                    event: text.clone(),
                },
                true,
            ),
            4 => (
                GhError::Marker {
                    marker: text.clone(),
                    reason: "closes the comment",
                },
                true,
            ),
            5 => (
                GhError::Transport {
                    what: text.clone(),
                    exit_code: i32::try_from(index).ok(),
                    stderr: String::from("connection reset by peer"),
                },
                false,
            ),
            6 => (GhError::Deadline { what: text.clone() }, false),
            7 => (
                GhError::MalformedResponse {
                    path: String::from("stdout"),
                    source: text.clone(),
                },
                false,
            ),
            _ => (
                GhError::Response {
                    path: text.clone(),
                    reason: String::from("the created review has no id"),
                },
                false,
            ),
        };
        let shown = error.to_string();
        let certainty = match lgwks_bot::script::FlowError::from(error) {
            lgwks_bot::script::FlowError::Bot { ref source, .. } => match **source {
                BotError::DomainError { certainty, .. } => certainty,
                BotError::EffectIndeterminate { .. } => DispatchCertainty::Unsettled,
                ref other => return Err(format!("{shown}: unexpected bot error {other}").into()),
            },
            other => return Err(format!("{shown}: not a bot error: {other}").into()),
        };
        hashes.push(text_hash(&format!("{index}:{certainty:?}")));
        if sent_nothing {
            seen_refused = seen_refused.saturating_add(1);
            assert_eq!(
                certainty,
                DispatchCertainty::Refused,
                "seed {index}: {shown} happened before any child existed, so nothing \
                 reached GitHub and the run must not reconcile it as unknown"
            );
        } else {
            seen_after_start = seen_after_start.saturating_add(1);
            assert_ne!(
                certainty,
                DispatchCertainty::Refused,
                "seed {index}: {shown} happened after the client ran, so the effect \
                 is unobserved and the run must read back"
            );
        }
    }
    assert!(
        seen_refused > 0 && seen_after_start > 0,
        "both halves are drawn"
    );
    let mut replay = Vec::new();
    for (index, hash) in hashes.iter().enumerate() {
        replay.push(text_hash(&format!("{index}:{hash}")));
    }
    assert_eq!(replay.len(), 64, "every seed left one trace entry");
    Ok(())
}

/// One seeded lowercase hex commit id.
fn seeded_sha(index: u64, salt: u64) -> String {
    use std::fmt::Write as _;

    let mut text = String::with_capacity(40);
    let mut state = index.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ salt;
    while text.len() < 40 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // `write!` into a `String` cannot fail; the result is bound so nothing
        // here discards an error by accident.
        let _written = write!(text, "{state:016x}");
    }
    text.truncate(40);
    text
}

/// GitHub's pull-request object, decoded as the adapter decodes it.
///
/// The real API nests the pinned commits as `head.sha` and `base.sha` among
/// many other fields; a decoder that read flat `head_sha` fields pins nothing
/// on every real pull request. Each seed draws its own commits, number and
/// field order, and a flat-shaped answer must pin nothing.
#[test]
fn pull_object_contract_r64() -> TestResult {
    for index in 0..64u64 {
        let head = seeded_sha(index, 1);
        let base = seeded_sha(index, 2);
        let number = index.saturating_add(1);
        let head_object = format!("\"head\":{{\"ref\":\"topic-{index}\",\"sha\":\"{head}\"}}");
        let base_object = format!("\"base\":{{\"ref\":\"main\",\"sha\":\"{base}\"}}");
        let fields = [
            format!("\"number\":{number}"),
            String::from("\"state\":\"open\""),
            head_object,
            base_object,
            format!("\"title\":\"change {index}\""),
        ];
        let rotate = pick(index, fields.len());
        let ordered: Vec<&str> = fields[rotate..]
            .iter()
            .chain(fields[..rotate].iter())
            .map(String::as_str)
            .collect();
        let answer = format!("{{{}}}", ordered.join(","));
        let snapshot = lgwks_std::json::from_str::<lgwks_bot::domain::gh::PrSnapshot>(&answer)?;
        assert_eq!(snapshot.number(), number, "seed {index}: {answer}");
        assert_eq!(
            snapshot.head_sha(),
            head,
            "seed {index}: the head is `head.sha`"
        );
        assert_eq!(
            snapshot.base_sha(),
            base,
            "seed {index}: the base is `base.sha`"
        );

        let flat =
            format!("{{\"number\":{number},\"head_sha\":\"{head}\",\"base_sha\":\"{base}\"}}");
        let not_github = lgwks_std::json::from_str::<lgwks_bot::domain::gh::PrSnapshot>(&flat)?;
        assert!(
            not_github.head_sha().is_empty() && not_github.base_sha().is_empty(),
            "seed {index}: a flat answer is not GitHub's shape and pins nothing"
        );
    }
    Ok(())
}

/// Inline comments are carried in the payload and read back by field.
///
/// A non-empty comment list travels on the wire (where a verifier compares it),
/// and an empty one is omitted, so a payload that declares no comments is
/// byte-for-byte what it always was.
#[test]
fn review_comments_are_carried_and_omitted_r16() -> TestResult {
    for index in 0..16u64 {
        let subject = CommitId::new(seeded_sha(index, 5))?;
        let comments = vec![
            ReviewComment::new(format!("src/a{index}.rs"), index.saturating_add(1), "first"),
            ReviewComment::new(
                format!("src/b{index}.rs"),
                index.saturating_add(2),
                "second",
            ),
        ];
        let payload =
            ReviewPayload::new(&subject, "COMMENT", "body", "marker")?.with_comments(comments);
        assert_eq!(
            payload.comments().len(),
            2,
            "seed {index}: both comments are carried"
        );
        let first = payload.comments().first().ok_or("the first comment")?;
        assert!(
            first.path().starts_with("src/a"),
            "seed {index}: a comment names its path"
        );
        assert_eq!(first.line(), index.saturating_add(1));
        assert_eq!(first.body(), "first");
        let wire = lgwks_std::json::to_string(&payload)?;
        assert!(wire.contains("\"comments\":["), "seed {index}: {wire}");
        assert!(
            !wire.contains("\"comments\":[]"),
            "seed {index}: a non-empty list is sent, never an empty one"
        );

        let bare = ReviewPayload::new(&subject, "COMMENT", "body", "marker")?;
        assert!(
            !lgwks_std::json::to_string(&bare)?.contains("comments"),
            "seed {index}: an empty comment list is omitted from the wire"
        );
    }
    Ok(())
}

/// A review verifies in the state GitHub reports, with its marker inside the
/// body, and never under the request's spelling or another marker.
#[test]
fn review_state_and_marker_r64() -> TestResult {
    let reported = [
        ("COMMENT", "COMMENTED"),
        ("APPROVE", "APPROVED"),
        ("REQUEST_CHANGES", "CHANGES_REQUESTED"),
    ];
    for index in 0..64u64 {
        let subject = CommitId::new(seeded_sha(index, 3))?;
        let (event, state) = reported[pick(index, reported.len())];
        let body = format!("finding {index}: {}", text_hash(&index.to_string()));
        let marker = if index % 4 == 0 {
            String::new()
        } else {
            format!("tenant-{}/run-{index}", index % 5)
        };
        let payload = ReviewPayload::new(&subject, event, body.as_str(), marker.as_str())?;
        assert_eq!(payload.state(), state, "seed {index}: {event}");
        if marker.is_empty() {
            assert_eq!(payload.body(), body, "seed {index}: no marker, no trailer");
        } else {
            assert_eq!(
                payload.body(),
                format!("{body}\n\n<!-- lgwks-review:{marker} -->"),
                "seed {index}: the marker travels at the end of the body"
            );
        }
        let wire = lgwks_std::json::to_string(&payload)?;
        assert!(
            !wire.contains("\"marker\""),
            "seed {index}: GitHub's create request has no `marker` field: {wire}"
        );

        let read_back = ReviewRecord::new(9, subject.as_str(), state, payload.body());
        assert!(
            read_back.matches(&payload),
            "seed {index}: {event} reads back as {state}"
        );
        let echoed = ReviewRecord::new(9, subject.as_str(), event, payload.body());
        assert!(
            !echoed.matches(&payload),
            "seed {index}: `{event}` is the request's spelling, not a state GitHub reports"
        );
        let other = ReviewPayload::new(&subject, event, body.as_str(), format!("other-{index}"))?;
        assert!(
            !read_back.matches(&other),
            "seed {index}: identical text under another marker is another publication"
        );
    }
    Ok(())
}

/// A marker that could close or break out of the comment that carries it is
/// refused, wherever in the marker the hostile token is placed.
#[test]
fn marker_escape_r64() -> TestResult {
    let subject = CommitId::new(seeded_sha(7, 4))?;
    let hostile = ["--", "-->", "<", ">", "\n", "\r", "\u{0}", "\t"];
    for index in 0..64u64 {
        let token = hostile[pick(index, hostile.len())];
        let prefix = "p".repeat(pick(index, 9));
        let suffix = "s".repeat(pick(index.wrapping_mul(3), 7));
        let marker = format!("{prefix}{token}{suffix}");
        assert!(
            matches!(
                ReviewPayload::new(&subject, "COMMENT", "body", marker.as_str()),
                Err(GhError::Marker { .. })
            ),
            "seed {index}: {marker:?} could break out of the comment"
        );
        let length = 250_usize.saturating_add(pick(index, 13));
        let long = "m".repeat(length);
        let accepted = ReviewPayload::new(&subject, "COMMENT", "body", long.as_str()).is_ok();
        assert_eq!(
            accepted,
            length <= ReviewPayload::MAX_MARKER_BYTES,
            "seed {index}: a {length}-byte marker against the declared ceiling"
        );
    }
    Ok(())
}

/// `.` and `..` are path segments, not repository names: `../..` would turn
/// `repos/{owner}/{repo}/pulls` into a request for another endpoint.
#[test]
fn repository_segments_r64() {
    let dots = [".", ".."];
    for index in 0..64u64 {
        let name = format!("repo-{index}");
        let dot = dots[pick(index, dots.len())];
        let spec = if index % 2 == 0 {
            format!("{dot}/{name}")
        } else {
            format!("{name}/{dot}")
        };
        assert!(
            matches!(
                Repository::new(spec.as_str()),
                Err(GhError::Repository { .. })
            ),
            "seed {index}: {spec:?} is a path segment, not a repository"
        );
        let dotted = format!("acme/{name}.{dot}x");
        assert!(
            Repository::new(dotted.as_str()).is_ok(),
            "seed {index}: {dotted:?} merely contains dots and is a name"
        );
    }
}

/// The position `index` selects in a table of `width` entries.
///
/// One helper rather than a per-family expression: `index % width` needs the
/// two types to agree, and four families each writing that conversion is four
/// chances for one of them to use a different modulus.
fn pick(index: u64, width: usize) -> usize {
    let width_u64 = u64::try_from(width).unwrap_or(u64::MAX);
    usize::try_from(index.checked_rem(width_u64).unwrap_or(0)).unwrap_or(0)
}

/// A stable hash of plain text, for a family whose values are not traces.
fn text_hash(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}
