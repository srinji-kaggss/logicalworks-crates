// File: crates/lgwks-bot/src/review.rs (rust)
//! The canonical PR-review task: pin a subject, read a snapshot, produce a
//! review for it, publish it at the reviewed commit, and verify the
//! publication independently.
//!
//! # The whole safety argument, in one place
//!
//! Every defect this module exists to prevent is a case of **the review's
//! subject drifting away from the code that was read**, or **an effect being
//! repeated because its outcome was not observed**. Each is answered by one
//! mechanism rather than by care at the call site:
//!
//! | Failure | Mechanism | Type |
//! |---|---|---|
//! | the head moved between snapshot and publication | [`ReviewOutcome::TargetMoved`] naming reviewed and current | refusal, publishes nothing |
//! | a lost response after an accepted write | one read-back, never a second create | [`ReviewOutcome::Published`] with `verified: true` |
//! | a lost response that cannot be reconciled | the outcome stays open | [`ReviewOutcome::Unknown`] |
//! | a marker matching but the payload differing | [`ReviewRecord::matches`] compares subject, body and state | refusal |
//! | an unbounded or chatty `gh` | the adapter's capture ceiling and deadline | [`domain::gh::GhOutcome`] |
//!
//! # What this module deliberately does not contain
//!
//! No process id, no reap loop, no retry ladder over publication, no queue and
//! no private state machine. Every effect is one call on the adapter, which is
//! the crate's only sanctioned runner; every wait is
//! [`within`](crate::script::within); and the run is bounded by the host. That
//! is what makes the task body ordinary business code rather than a second
//! orchestrator.
//!
//! # Ephemeral by construction
//!
//! This is the **local** mode: the run holds no durable record between
//! attempts, so a lost response is reconciled by reading back within the same
//! run and never by consulting a journal the mode does not have. The
//! [`ReviewOutcome::Unknown`] arm is what makes that limitation visible instead
//! of papering over it: with no durable store to re-read, a write whose outcome
//! cannot be observed from GitHub is reported as unknown and the caller
//! decides. A durable mode that persists the same
//! [`ReviewIntent`](ReviewIntent) and replays it after a restart belongs to the
//! effect layer, not here.

use std::time::Duration;

use crate::domain::gh::{CommitId, Gh, PrSnapshot, PullRequest, ReviewPayload, ReviewRecord};
use crate::script::{FlowError, Scope, within};

// ── The request ─────────────────────────────────────────────────────────────

/// What one review run is asked to do.
///
/// The subject is a pull request, not a commit: pinning happens here, from the
/// first read, so every later comparison is against a value this request
/// captured rather than against whatever the endpoint currently reports.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReviewRequest {
    /// The pull request to review.
    pull: PullRequest,
    /// The review body to publish.
    #[expect(
        dead_code,
        reason = "the request's body is supplied by the caller's script, which \
        composes the body it wants to publish; the field is the record of what was asked for and \
        is read by `body()`"
    )]
    body: String,
    /// The review event: `COMMENT`, `APPROVE` or `REQUEST_CHANGES`.
    #[expect(
        dead_code,
        reason = "read through `event()`; the field exists so a request carries \
        its own policy rather than having the caller keep it beside it"
    )]
    event: String,
    /// An application marker used only to *locate* a candidate review during
    /// reconciliation.
    ///
    /// Deliberately not a uniqueness proof: two runs of the same request carry
    /// the same marker, and it is [`ReviewRecord::matches`] that decides.
    #[expect(
        dead_code,
        reason = "read through `marker()`; kept private so a caller cannot edit \
        the payload after the request was declared"
    )]
    marker: String,
    /// How long each network step may take before the step is abandoned.
    ///
    /// Private with an accessor so the bound a run was declared with is what
    /// it runs under, not what a caller can change halfway.
    ///
    /// A deadline, not a cancellation: an abandoned step drops its future, so
    /// the child process it was driving is stopped by its own adapter deadline
    /// rather than left running.
    pub step_deadline: Duration,
}

impl ReviewRequest {
    /// A request for `pull` publishing `body` as `event`.
    #[must_use]
    pub fn new(pull: PullRequest, event: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            pull,
            body: body.into(),
            event: event.into(),
            // Derived from the step key at run time by [`review_pr`]; an empty
            // marker here is never sent, because the body is built there.
            marker: String::new(),
            step_deadline: Duration::from_secs(120),
        }
    }

    /// Locate candidate reviews with `marker` during reconciliation.
    ///
    /// The marker only narrows the search. Verification still compares the
    /// subject commit, the body and the state.
    #[must_use]
    pub fn with_marker(mut self, marker: impl Into<String>) -> Self {
        self.marker = marker.into();
        self
    }

    /// Bound every step of the run to `deadline`.
    #[must_use]
    pub fn step_deadline(mut self, deadline: Duration) -> Self {
        self.step_deadline = deadline;
        self
    }

    /// The pull request this request names.
    #[must_use]
    pub const fn pull(&self) -> &PullRequest {
        &self.pull
    }

    /// The review body the request asked for.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// The review event the request asked for.
    #[must_use]
    pub fn event(&self) -> &str {
        &self.event
    }

    /// The reconciliation marker.
    #[must_use]
    pub fn marker(&self) -> &str {
        &self.marker
    }

    /// How long each step of the run may take.
    #[must_use]
    pub const fn step_budget(&self) -> Duration {
        self.step_deadline
    }
}

// ── The outcome ─────────────────────────────────────────────────────────────

/// What one review run published, proved, or refused.
///
/// The variants are the states the journey file names, and each is a fact a
/// caller can act on without reading a message: a verified publication, a
/// publication whose effect is still unknown, a target that moved, a subject
/// the run refused to review, and a capability that was missing.
///
/// `#[non_exhaustive]`: the set of states a review can be in grows with what
/// the adapter can observe, and a consumer matching on it must keep a wildcard
/// arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReviewOutcome {
    /// The review was created and an independent read-back found it, about the
    /// reviewed commit, saying the reviewed thing.
    Published {
        /// The review id GitHub assigned.
        review_id: u64,
        /// The commit the review was published at.
        commit_id: CommitId,
        /// Always `true` in this variant. The field is carried so a caller
        /// reading one shape does not infer verification from the variant, and
        /// so a future variant can carry an unverified publication without
        /// changing this type.
        verified: bool,
    },
    /// A create may have been accepted and its response lost, and the
    /// read-back could not establish whether it landed.
    ///
    /// Reported rather than retried. A second create here is the duplicate
    /// review this module exists to prevent, and with no durable record of the
    /// first attempt there is nothing that would make it safe.
    Unknown {
        /// The commit the attempted review was about.
        commit_id: CommitId,
        /// What the read-back said, verbatim from the adapter.
        reason: String,
    },
    /// The head was no longer the pinned commit when the run checked, so
    /// nothing was published.
    ///
    /// Not a failure of the review and not a silent re-subject: the code that
    /// was read is still exactly what this review says, and the head that
    /// replaced it has not been read at all.
    TargetMoved {
        /// The commit this run read and reviewed.
        reviewed: CommitId,
        /// The commit the pull request pointed at when the run checked.
        current: String,
    },
    /// The review was drafted but not published, and the reason is one the
    /// caller can act on without re-running anything.
    Refused {
        /// What was refused, in the adapter's own words.
        reason: String,
    },
}

impl ReviewOutcome {
    /// Whether the run published a review it then verified.
    #[must_use]
    pub const fn is_published(&self) -> bool {
        matches!(self, Self::Published { .. })
    }

    /// The review id, when this outcome names one.
    #[must_use]
    pub const fn review_id(&self) -> Option<u64> {
        match *self {
            Self::Published { review_id, .. } => Some(review_id),
            Self::Unknown { .. } | Self::TargetMoved { .. } | Self::Refused { .. } => None,
        }
    }

    /// The commit the published or attempted review was about.
    #[must_use]
    pub fn commit_id(&self) -> Option<&CommitId> {
        match *self {
            Self::Published { ref commit_id, .. } | Self::Unknown { ref commit_id, .. } => {
                Some(commit_id)
            }
            Self::TargetMoved { ref reviewed, .. } => Some(reviewed),
            Self::Refused { .. } => None,
        }
    }

    /// Whether the external effect is still unknown.
    ///
    /// `true` only for [`ReviewOutcome::Unknown`]. A caller deciding whether it
    /// may retry reads this rather than matching the whole enum, so a new
    /// variant cannot silently inherit "safe to retry".
    #[must_use]
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown { .. })
    }
}

// ── The task body ───────────────────────────────────────────────────────────

/// Review `request.pull`, publish the review, and report what happened.
///
/// The body is ordinary business code: read the subject, confirm the subject
/// has not moved, publish at that commit, read back and compare. It manages no
/// process, holds no pid, and contains no retry over the publication — the one
/// operation where a retry is not safe.
///
/// The sequence, and why each step is where it is:
///
/// 1. **Snapshot** — reads the pull request's head and base. This is what the
///    review is pinned to.
/// 2. **Script** — the caller-supplied analysis over the snapshot, which
///    produces the body to publish. Kept separate from the publication so a
///    script failure cannot leave a half-published review.
/// 3. **Freshness** — re-reads the head and refuses if it moved. This is what
///    makes "the review is about the code that was read" a checked property
///    rather than an intention.
/// 4. **Publish** — one create at the reviewed commit.
/// 5. **Verify** — a separate read-back. A successful create is transport
///    evidence; only this is publication evidence.
/// 6. **Reconcile** — if step 4's response was lost, read back once more for
///    the review this run intended. A second create is never issued.
#[cfg(feature = "process")]
pub async fn review_pr(
    scope: Scope,
    gh: Gh,
    request: ReviewRequest,
    script: impl Fn(&PrSnapshot, &Scope) -> ReviewScript,
) -> Result<ReviewOutcome, FlowError> {
    let deadline = request.step_deadline;

    // 1. Pin the subject. Every later comparison is against this read.
    let snapshot = within(&scope, "snapshot", deadline, async {
        gh.snapshot(&request.pull).await.map_err(FlowError::from)
    })
    .await?;

    let reviewed = CommitId::new(snapshot.head_sha().to_owned()).map_err(|source| {
        FlowError::failed(format!(
            "the pull request's head is not a commit id: {source}"
        ))
    })?;

    // 2. Run the caller's analysis over the pinned snapshot. A script that
    // fails here has published nothing, which is the point of running it
    // before the publication stage rather than inside it.
    let step = scope.enter("script")?;
    let body = script(&snapshot, &step).map_err(FlowError::from);
    drop(step);
    let body = body?;

    // The marker travels in the body so a read-back can *locate* this review.
    // It is a locator: [`ReviewRecord::matches`] decides.
    let payload = ReviewPayload::new(&reviewed, &request.event, body, &request.marker)
        .map_err(|source| FlowError::failed(source.to_string()))?;

    // 3. The head must still be what was read. A refusal here publishes
    // nothing and names both shas, so the caller can decide whether to
    // re-analyse at the new head — which is a separate recorded decision.
    let current = within(&scope, "freshness", deadline, async {
        gh.snapshot(&request.pull).await.map_err(FlowError::from)
    })
    .await?;

    if current.head_sha() != reviewed.as_str() {
        return Ok(ReviewOutcome::TargetMoved {
            reviewed,
            current: current.head_sha().to_owned(),
        });
    }

    // 4. Publish, once, at the reviewed commit.
    let published = within(&scope, "publish", deadline, async {
        gh.publish(&request.pull, &payload)
            .await
            .map_err(FlowError::from)
    })
    .await;

    let created_id = match published {
        Ok(id) => Some(id),
        Err(error) => {
            // A refusal *before* the fork established that nothing ran, so a
            // retry is a retry. An error *after* the fork may mean the review
            // landed: those two are different facts and the task must not
            // collapse them. The reconciliation read below is what separates
            // them, by observing GitHub rather than by trusting an exit code.
            if !may_have_landed(&error) {
                return Err(error);
            }
            None
        }
    };

    // 5. Verify, whether or not the create answered. A lost response is
    //    reconciled by this same read — never by a second create.
    let reviews = within(&scope, "verify", deadline, async {
        gh.read_reviews(&request.pull)
            .await
            .map_err(FlowError::from)
    })
    .await;

    let reviews = match reviews {
        Ok(records) => records,
        // The write may have landed and the read could not confirm it. This is
        // exactly the state that must not be reported as a failure and must
        // not be retried blindly.
        Err(error) => {
            return Ok(ReviewOutcome::Unknown {
                commit_id: reviewed,
                reason: format!(
                    "the publication outcome could not be established: {error}; \
                     no second review was created"
                ),
            });
        }
    };

    match find_verified(&reviews, &payload, created_id) {
        Some(id) => Ok(ReviewOutcome::Published {
            review_id: id,
            commit_id: reviewed,
            verified: true,
        }),
        None if created_id.is_some() => {
            // The create returned an id, and the read-back did not find it. A
            // successful create whose review cannot be found is a contradiction,
            // not a success: it is reported, never "fixed" by re-posting.
            Err(FlowError::failed(format!(
                "GitHub accepted review {} at {} but the read-back did not return it; \
                 no second review was created",
                created_id.unwrap_or_default(),
                reviewed
            )))
        }
        None => Ok(ReviewOutcome::Unknown {
            commit_id: reviewed,
            reason: String::from(
                "the publication response was lost and the read-back found no matching review; \
                 the review may or may not exist, so nothing was created again",
            ),
        }),
    }
}

/// Whether a failed publish step leaves the review's existence unknown.
///
/// The distinction is the whole of PR-07. A step that failed before the fork
/// proved nothing ran. A step that failed after — or whose response was lost —
/// left the effect unobserved, and the only sound next move is a read.
#[cfg(feature = "process")]
fn may_have_landed(error: &FlowError) -> bool {
    match *error {
        // `within` itself did not time out and the scope was not stopped: the
        // adapter reported the failure, so what it reported is what is known.
        FlowError::Bot { ref source, .. } => {
            matches!(**source, crate::error::BotError::EffectIndeterminate { .. })
        }
        // A stop or a deadline during the publish step: the child may have been
        // written and the response dropped, so this is unknown, not "not done".
        FlowError::TimedOut { .. } | FlowError::Cancelled { .. } => true,
        _ => false,
    }
}

/// Find the review `payload` describes, preferring the id a create returned.
///
/// The comparison is the verification: same subject commit, same body, same
/// state. The application marker is deliberately absent — a matching marker
/// locates a candidate, and treating it as proof is the error PR-07 names.
#[cfg(feature = "process")]
fn find_verified(
    reviews: &[ReviewRecord],
    payload: &ReviewPayload,
    created_id: Option<u64>,
) -> Option<u64> {
    // A create that returned an id is matched against that exact record first:
    // if GitHub says this id exists and it says the wrong thing, the read-back
    // contradicts the write and no other record is substituted for it.
    if let Some(id) = created_id
        && let Some(record) = reviews.iter().find(|record| record.id() == id)
    {
        return record.matches(payload).then_some(id);
    }
    // Otherwise: the lost-response path. Find any review that is about this
    // commit, says this body, and is in this state. A review of another commit
    // or another body is not this publication, however similar its marker.
    reviews
        .iter()
        .find(|record| record.matches(payload))
        .map(ReviewRecord::id)
}

// ── The script type ─────────────────────────────────────────────────────────

/// The caller's analysis stage: turn a pinned snapshot into the body to publish.
///
/// A function rather than a trait object so the task stays generic over the
/// analysis and the caller's closure keeps its own captured state without a
/// `Box`. It returns the body directly, so a script cannot silently fail to
/// produce one: the only way to fail is by not running.
#[cfg(feature = "process")]
pub type ReviewScript = std::result::Result<String, ScriptFailure>;

/// Why an analysis stage did not produce a review body.
///
/// Separate from [`FlowError`] because a script failure is the *script's*
/// verdict, not an orchestration failure: the run failed for a reason the
/// caller wrote, located in their code, and a caller that reads it should not
/// have to distinguish it from a transport failure to know that nothing was
/// published.
#[cfg(feature = "process")]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScriptFailure {
    /// What the analysis said.
    reason: String,
}

#[cfg(feature = "process")]
impl ScriptFailure {
    /// Name why the analysis produced no review.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// What the analysis said.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

#[cfg(feature = "process")]
impl std::fmt::Display for ScriptFailure {
    /// The reason as written.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.reason)
    }
}

#[cfg(feature = "process")]
impl std::error::Error for ScriptFailure {}

#[cfg(feature = "process")]
impl From<ScriptFailure> for FlowError {
    /// A script failure is a located permanent failure: repeating the analysis
    /// gives the same answer, and nothing was published to repeat.
    fn from(source: ScriptFailure) -> Self {
        FlowError::failed(source.reason)
    }
}
