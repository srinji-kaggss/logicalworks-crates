// The canonical PR-review task: pin a subject, read a snapshot, produce a
// review for it, publish it at the reviewed commit, and verify the
// publication independently.
//
// # The whole safety argument, in one place
//
// Every defect this module exists to prevent is a case of **the review's
// subject drifting away from the code that was read**, or **an effect being
// repeated because its outcome was not observed**. Each is answered by one
// mechanism rather than by care at the call site:
//
// | Failure | Mechanism | Type |
// |---|---|---|
// | the head moved between snapshot and publication | [`ReviewOutcome::TargetMoved`] naming reviewed and current | refusal, publishes nothing |
// | a lost response after an accepted write | one read-back, never a second create | [`ReviewOutcome::Published`] with `verified: true` |
// | a lost response that cannot be reconciled | the outcome stays open | [`ReviewOutcome::Unknown`] |
// | a marker matching but the payload differing | [`ReviewRecord::matches`](crate::domain::gh::ReviewRecord::matches) compares subject, body and state | refusal |
// | an unbounded or chatty `gh` | the adapter's capture ceiling and deadline | [`domain::gh::GhOutcome`] |
//
// # What this module deliberately does not contain
//
// No process id, no reap loop, no retry ladder over publication, no queue and
// no private state machine. Every effect is one call on the adapter, which is
// the crate's only sanctioned runner; every wait is
// [`within`](crate::script::within); and the run is bounded by the host. That
// is what makes the task body ordinary business code rather than a second
// orchestrator.
//
// # Ephemeral by construction
//
// This is the **local** mode: the run holds no durable record between
// attempts, so a lost response is reconciled by reading back within the same
// run and never by consulting a journal the mode does not have. The
// [`ReviewOutcome::Unknown`] arm is what makes that limitation visible instead
// of papering over it: with no durable store to re-read, a write whose outcome
// cannot be observed from GitHub is reported as unknown and the caller
// decides. A durable mode that persists the same
// `ReviewIntent`(ReviewIntent) and replays it after a restart belongs to the
// effect layer, not here.

use std::time::Duration;

use crate::domain::gh::{
    CommitId, Gh, GhError, PrSnapshot, PullRequest, ReviewComment, ReviewPayload, ReviewRecord,
};
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
    ///
    /// Read through [`ReviewRequest::body`]. The field rather than nothing so a
    /// request is the whole of what was asked for: a caller that builds one
    /// does not have to keep the body beside it and hope the two agree.
    body: String,
    /// The review event: `COMMENT`, `APPROVE` or `REQUEST_CHANGES`.
    ///
    /// Read through [`ReviewRequest::event`]. The review policy belongs to the
    /// request, not to a value the caller remembers separately.
    event: String,
    /// The inline comments to publish, in order.
    ///
    /// Each is a distinct remote operation from the top-level body (PR-08), so
    /// they travel as a `comments` array. A review whose body landed and whose
    /// comments were accepted only in part is a *partial* submission the
    /// reconciliation reports rather than a match. Empty by default, so a
    /// request that declares none behaves exactly as before.
    comments: Vec<ReviewComment>,
    /// An application marker used only to *locate* a candidate review during
    /// reconciliation.
    ///
    /// Deliberately not a uniqueness proof: two runs of the same request carry
    /// the same marker, and it is [`ReviewRecord::matches`](crate::domain::gh::ReviewRecord::matches) that decides.
    ///
    /// Read through [`ReviewRequest::marker`], and private so the payload
    /// cannot be edited after the request it belongs to was declared.
    marker: String,
    /// How long each network step may take before the step is abandoned.
    ///
    /// Private with an accessor so the bound a run was declared with is what
    /// it runs under, not what a caller can change halfway.
    ///
    /// A deadline, not a cancellation: an abandoned step drops its future, so
    /// the child process it was driving is stopped by its own adapter deadline
    /// rather than left running.
    step_deadline: Duration,
}

impl ReviewRequest {
    /// A request for `pull` publishing `body` as `event`.
    #[must_use]
    pub fn new(pull: PullRequest, event: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            pull,
            body: body.into(),
            event: event.into(),
            comments: Vec::new(),
            // Empty means "derive it": [`review_pr`] uses the run's step key,
            // which is distinct per tenant and step path and stable across a
            // re-run of the same step — an idempotency key, not a nonce.
            marker: String::new(),
            step_deadline: Duration::from_secs(120),
        }
    }

    /// Publish these inline comments with the review.
    ///
    /// A comment is a distinct remote operation from the top-level body, so a
    /// review whose comments were accepted only in part is a partial submission
    /// the journey reports as its own state rather than folding into a verified
    /// publication.
    #[must_use]
    pub fn with_comments(mut self, comments: Vec<ReviewComment>) -> Self {
        self.comments = comments;
        self
    }

    /// Carry `marker` in the published body instead of the run's step key.
    ///
    /// The marker travels inside the body, so verification — which compares
    /// the subject commit, the whole body and the state — distinguishes two
    /// requests whose text is identical but whose markers differ.
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

    /// The inline comments the request will publish.
    #[must_use]
    pub fn comments(&self) -> &[ReviewComment] {
        &self.comments
    }

    /// The hidden trailer a published review carries, which a later run reads to recognise its own earlier review.
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
    /// The subject was pinned, but its coverage could not be completed, so
    /// nothing was published.
    ///
    /// An unavailable or over-ceiling diff is a *coverage* decision, never a
    /// clean review: the code that was read is only part of what the pull
    /// request changes, and a review reported against a partial diff would
    /// claim a scope nobody read. This is a state a caller can act on — the
    /// subject is known, nothing was published — not a run failure.
    Incomplete {
        /// The commit the run read before the coverage failed.
        commit_id: CommitId,
        /// Why the coverage is incomplete, in the adapter's own words.
        reason: String,
    },
    /// A create landed as an **unsubmitted draft**: the receiver holds a review
    /// in `PENDING` about the reviewed commit, and no review was submitted.
    ///
    /// Distinct from [`ReviewOutcome::Published`] (nothing was submitted) and
    /// from [`ReviewOutcome::Unknown`] (the review's existence *is* established,
    /// but it is a draft). No second create is issued.
    Pending {
        /// The id of the pending review the read-back found.
        review_id: u64,
        /// The commit the pending review is about.
        commit_id: CommitId,
    },
    /// A submitted review landed with only part of its inline comments.
    ///
    /// The top-level review is real and submitted about the reviewed commit, but
    /// fewer comments landed than were intended. Reported with both counts so a
    /// caller can target only what remains, rather than re-posting the whole
    /// review — which is the duplicate this module exists to prevent.
    Partial {
        /// The id of the partially submitted review.
        review_id: u64,
        /// The commit the review is about.
        commit_id: CommitId,
        /// How many inline comments landed.
        applied: usize,
        /// How many the run intended.
        intended: usize,
    },
    /// A create returned an id — the effect is applied — but an independent
    /// read-back could not verify it, typically because read permission was
    /// lost.
    ///
    /// The applied-effect evidence (the review id) is **retained**: a caller
    /// knows a review exists and which one, and can reconcile it once access is
    /// restored. No second create is issued.
    Unverified {
        /// The id the create returned.
        review_id: u64,
        /// The commit the review is about.
        commit_id: CommitId,
        /// Why the verification could not be completed, in the adapter's words.
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
            Self::Published { review_id, .. }
            | Self::Pending { review_id, .. }
            | Self::Partial { review_id, .. }
            | Self::Unverified { review_id, .. } => Some(review_id),
            Self::Unknown { .. }
            | Self::TargetMoved { .. }
            | Self::Refused { .. }
            | Self::Incomplete { .. } => None,
        }
    }

    /// The commit the published or attempted review was about.
    #[must_use]
    pub fn commit_id(&self) -> Option<&CommitId> {
        match *self {
            Self::Published { ref commit_id, .. }
            | Self::Unknown { ref commit_id, .. }
            | Self::Incomplete { ref commit_id, .. }
            | Self::Pending { ref commit_id, .. }
            | Self::Partial { ref commit_id, .. }
            | Self::Unverified { ref commit_id, .. } => Some(commit_id),
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

    /// Whether an applied effect's verification could not be completed.
    #[must_use]
    pub const fn is_unverified(&self) -> bool {
        matches!(self, Self::Unverified { .. })
    }

    /// Whether the create landed as an unsubmitted draft.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }

    /// Whether a submitted review landed with only part of its comments.
    #[must_use]
    pub const fn is_partial(&self) -> bool {
        matches!(self, Self::Partial { .. })
    }

    /// Whether the subject's coverage could not be completed.
    #[must_use]
    pub const fn is_incomplete(&self) -> bool {
        matches!(self, Self::Incomplete { .. })
    }

    /// Why the publication outcome could not be established, when it was not.
    ///
    /// `None` for every other variant. Present because an `Unknown` is a
    /// *successful* run whose report carries no located error at all: the
    /// information a caller needs is in the outcome, and a consumer that only
    /// read `Report::error` would find nothing there and have to conclude the
    /// run said nothing at all. It is a fact about the outcome, not about
    /// execution, which is why it lives here and not on the report.
    #[must_use]
    pub fn unknown_reason(&self) -> Option<&str> {
        match *self {
            Self::Unknown { ref reason, .. } => Some(reason),
            Self::Published { .. }
            | Self::TargetMoved { .. }
            | Self::Refused { .. }
            | Self::Incomplete { .. }
            | Self::Pending { .. }
            | Self::Partial { .. }
            | Self::Unverified { .. } => None,
        }
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
    mut request: ReviewRequest,
    script: impl Fn(&PrSnapshot, &Scope) -> ReviewScript,
) -> Result<ReviewOutcome, FlowError> {
    let deadline = request.step_deadline;
    // An explicit marker wins; otherwise the run's step key, so two tenants
    // publishing the same text on one pull request can never adopt each
    // other's review during reconciliation.
    let marker = if request.marker.is_empty() {
        scope.key().to_string()
    } else {
        request.marker.clone()
    };
    // The comments are moved into the payload rather than copied: the request
    // is the only owner up to here, and the payload is where they are published
    // from.
    let comments = std::mem::take(&mut request.comments);

    // 1. Pin the subject. Every later comparison is against this read. A
    //    renamed repository answers with a move object rather than a pull
    //    request; that is a refusal of the *subject identity*, never a silent
    //    re-point to the canonical name.
    let snapshot = match within(&scope, "snapshot", deadline, async {
        match gh.snapshot(&request.pull).await {
            Ok(snapshot) => Ok(SnapshotStep::Pinned(snapshot)),
            Err(GhError::MovedRepository {
                requested,
                canonical,
            }) => Ok(SnapshotStep::Moved {
                requested,
                canonical,
            }),
            Err(other) => Err(FlowError::from(other)),
        }
    })
    .await?
    {
        SnapshotStep::Pinned(snapshot) => snapshot,
        SnapshotStep::Moved {
            requested,
            canonical,
        } => {
            return Ok(ReviewOutcome::Refused {
                reason: format!(
                    "the repository {requested} moved to {canonical}; the subject identity is \
                     not re-pointed, so nothing was reviewed or published under the canonical \
                     name"
                ),
            });
        }
    };

    let reviewed = CommitId::new(snapshot.head_sha().to_owned()).map_err(|source| {
        FlowError::failed(format!(
            "the pull request's head is not a commit id: {source}"
        ))
    })?;

    // 1b. Bind the changed-file inventory to the pinned pair. An unavailable or
    //     over-ceiling diff is an *incomplete coverage*: nothing may be
    //     published, because a review against a partial diff would claim a
    //     scope nobody read. The inventory is the subject's data — no file in
    //     it is compiled, imported, built, shelled or loaded.
    let diff = match within(&scope, "diff", deadline, async {
        match gh.read_diff(&request.pull).await {
            Ok(diff) => Ok(DiffStep::Read(diff)),
            Err(error) if error.is_coverage_incomplete() => {
                Ok(DiffStep::Incomplete(error.to_string()))
            }
            Err(other) => Err(FlowError::from(other)),
        }
    })
    .await?
    {
        DiffStep::Read(diff) => diff,
        DiffStep::Incomplete(reason) => {
            return Ok(ReviewOutcome::Incomplete {
                commit_id: reviewed,
                reason,
            });
        }
    };
    // The inventory is bound to the pinned subject; the read is the coverage
    // gate. Its bytes are data, never a program, and the run keeps none of them.
    drop(diff);

    // 2. Run the caller's analysis over the pinned snapshot. A script that
    // fails here has published nothing, which is the point of running it
    // before the publication stage rather than inside it.
    let step = scope.enter("script")?;
    let body = script(&snapshot, &step).map_err(FlowError::from);
    drop(step);
    let body = body?;

    // The marker travels in the body so a read-back can *locate* this review.
    // It is a locator: [`ReviewRecord::matches`](crate::domain::gh::ReviewRecord::matches) decides.
    let payload = ReviewPayload::new(&reviewed, &request.event, body, marker)
        .map_err(|source| FlowError::failed(source.to_string()))?
        .with_comments(comments);

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
            // The only failure that certainly wrote nothing is a refusal
            // *before the fork*. Every other publish failure — a non-zero exit,
            // a dropped connection, a deadline — may have reached GitHub and
            // had its response discarded, because the exit code alone cannot
            // distinguish "the request never arrived" from "the request was
            // applied and the answer was lost". Those two are different facts
            // and the task must not collapse them, so the only sound next move
            // is the reconciliation read below — which observes GitHub rather
            // than trusting an exit code. It is a read, never a second create.
            if !may_have_landed(&error) {
                let refusal = Err(error);
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "review_pr: returning an error to the caller");
                return refusal;
            }
            None
        }
    };

    // 5. Verify, whether or not the create answered. A lost response is
    //    reconciled by this same read — never by a second create.
    //
    //    A read that fails for any reason is *not* a run failure: the write may
    //    have landed, and reporting a failure would throw away the one fact the
    //    caller needs. The read's failure is classified instead — a *permission*
    //    loss is its own state (an applied effect whose verification is
    //    blocked), and every other refusal leaves the effect unknown.
    let verified = within(&scope, "verify", deadline, async {
        match gh.read_reviews(&request.pull).await {
            Ok(records) => Ok(VerifyStep::Read(records)),
            Err(GhError::Unauthorized { status, reason, .. }) => {
                Ok(VerifyStep::Denied { status, reason })
            }
            Err(other) => Ok(VerifyStep::Unreadable(other.to_string())),
        }
    })
    .await?;

    let reviews = match verified {
        VerifyStep::Read(records) => records,
        // The write may have landed and the read lost the permission to see it.
        // When the create returned an id, the effect is *applied*, so the
        // applied-effect evidence is retained rather than discarded: a caller
        // knows a review exists and which one, and can reconcile it once access
        // is restored. Otherwise the effect is unknown. Neither is a failure,
        // and neither is retried blindly.
        VerifyStep::Denied { status, reason } => {
            return Ok(match created_id {
                Some(review_id) => ReviewOutcome::Unverified {
                    review_id,
                    commit_id: reviewed,
                    reason: format!(
                        "the review was created but the read-back lost permission (HTTP \
                         {status}): {reason}; the applied review id is retained and no second \
                         review was created"
                    ),
                },
                None => ReviewOutcome::Unknown {
                    commit_id: reviewed,
                    reason: format!(
                        "the publication outcome could not be established, because the read \
                         lost permission (HTTP {status}): {reason}; no second review was created"
                    ),
                },
            });
        }
        // Any other unreadable answer leaves the effect unobserved. It is never
        // reported as a failure and never resolved by a second create.
        VerifyStep::Unreadable(reason) => {
            return Ok(ReviewOutcome::Unknown {
                commit_id: reviewed,
                reason: format!(
                    "the publication outcome could not be established: {reason}; \
                     no second review was created"
                ),
            });
        }
    };

    match reconcile(&reviews, &payload, created_id) {
        Reconcile::Verified(id) => Ok(ReviewOutcome::Published {
            review_id: id,
            commit_id: reviewed,
            verified: true,
        }),
        // A draft that landed is a distinct state, not a publication: it was
        // never submitted, and it is not "no effect" either.
        Reconcile::Pending(id) => Ok(ReviewOutcome::Pending {
            review_id: id,
            commit_id: reviewed,
        }),
        // A review whose comments landed only in part is reported with both
        // counts, so recovery targets only what remains rather than re-posting.
        Reconcile::Partial {
            id,
            applied,
            intended,
        } => Ok(ReviewOutcome::Partial {
            review_id: id,
            commit_id: reviewed,
            applied,
            intended,
        }),
        Reconcile::None if created_id.is_some() => {
            // The create returned an id, and the read-back either did not
            // return it or returned it saying something else. Either is a
            // contradiction, not a success: it is reported, never "fixed" by
            // re-posting.
            Err(FlowError::failed(format!(
                "GitHub accepted review {} at {} but the read-back did not return it as \
                 published; no second review was created",
                created_id.unwrap_or_default(),
                reviewed
            )))
        }
        Reconcile::None => Ok(ReviewOutcome::Unknown {
            commit_id: reviewed,
            reason: String::from(
                "the publication response was lost and the read-back found no matching review; \
                 the review may or may not exist, so nothing was created again",
            ),
        }),
    }
}

/// The outcome of the snapshot step, before the journey decides what to do.
///
/// A moved repository is a *decision* the journey reports, not a run failure, so
/// it is carried as a value rather than as a [`FlowError`] whose variant would
/// have to be recovered from a rendered string.
#[cfg(feature = "process")]
enum SnapshotStep {
    /// The pull request's head and base were read.
    Pinned(PrSnapshot),
    /// The repository moved; the canonical name is named, never followed.
    Moved {
        /// The repository the caller named.
        requested: String,
        /// The canonical repository the answer named.
        canonical: String,
    },
}

/// The outcome of the diff step, before the journey decides what to do.
#[cfg(feature = "process")]
enum DiffStep {
    /// The changed-file inventory was read within its declared bounds.
    Read(crate::domain::gh::PullDiff),
    /// The coverage could not be completed; the reason is the adapter's.
    Incomplete(String),
}

/// The outcome of the verification read, before the journey decides what to do.
///
/// A read failure is classified rather than propagated: a permission loss is
/// the *applied-effect-blocks-on-read* state T34 names, while every other
/// refusal leaves the effect unknown. Neither is a run failure — the write may
/// have landed, and a failure report would throw away the one fact the caller
/// needs.
#[cfg(feature = "process")]
enum VerifyStep {
    /// The review list was read.
    Read(Vec<ReviewRecord>),
    /// The read lost permission; the status and the adapter's reason.
    Denied {
        /// The HTTP status the client named.
        status: u16,
        /// The retained stderr.
        reason: String,
    },
    /// The read failed for any other reason.
    Unreadable(String),
}

/// What the reconciliation read established about this run's publication.
#[cfg(feature = "process")]
enum Reconcile {
    /// The exact intended review was observed, submitted and whole.
    Verified(u64),
    /// A review about the subject landed as an unsubmitted draft.
    Pending(u64),
    /// A submitted review landed with fewer inline comments than intended.
    Partial {
        /// The review id.
        id: u64,
        /// Comments that landed.
        applied: usize,
        /// Comments the run intended.
        intended: usize,
    },
    /// Nothing about this publication was established.
    None,
}

/// Find what `payload` describes among `reviews`, preferring an id a create
/// returned.
///
/// The comparison is the verification: same subject commit, same body, same
/// state, and the intended inline comments all landed. The application marker is
/// deliberately absent — a matching marker locates a candidate, and treating it
/// as proof is the error PR-07 names. A review that landed as an unsubmitted
/// draft, or submitted with only some comments, is recognised as its own state
/// rather than reported as a match or as nothing.
#[cfg(feature = "process")]
fn reconcile(
    reviews: &[ReviewRecord],
    payload: &ReviewPayload,
    created_id: Option<u64>,
) -> Reconcile {
    // A create that returned an id is matched against that exact record first:
    // if GitHub says this id exists and it says the wrong thing, the read-back
    // contradicts the write and no other record is substituted for it.
    if let Some(id) = created_id {
        let Some(record) = reviews.iter().find(|record| record.id() == id) else {
            return Reconcile::None;
        };
        if record.matches(payload) {
            return Reconcile::Verified(id);
        }
        if record.is_pending() && record.matches_subject_body(payload) {
            return Reconcile::Pending(id);
        }
        if record.matches_except_comments(payload) {
            return Reconcile::Partial {
                id,
                applied: record.applied_comments(),
                intended: payload.comments().len(),
            };
        }
        return Reconcile::None;
    }
    // Otherwise: the lost-response path. Find any review that is about this
    // commit, says this body, and is in this state. A review of another commit
    // or another body is not this publication, however similar its marker.
    if let Some(record) = reviews.iter().find(|record| record.matches(payload)) {
        return Reconcile::Verified(record.id());
    }
    if let Some(record) = reviews
        .iter()
        .find(|record| record.is_pending() && record.matches_subject_body(payload))
    {
        return Reconcile::Pending(record.id());
    }
    if let Some(record) = reviews
        .iter()
        .find(|record| record.matches_except_comments(payload))
    {
        return Reconcile::Partial {
            id: record.id(),
            applied: record.applied_comments(),
            intended: payload.comments().len(),
        };
    }
    Reconcile::None
}

/// Whether a failed publish step leaves the review's existence unknown.
///
/// The distinction is the whole of PR-07. A step that failed before the fork
/// proved nothing ran. A step that failed after — or whose response was lost —
/// left the effect unobserved, and the only sound next move is a read.
#[cfg(feature = "process")]
fn may_have_landed(error: &FlowError) -> bool {
    match *error {
        // A bot error carries the adapter's own certainty, and only one value
        // proves nothing was written: `Refused`, which is a refusal before the
        // fork. Every other certainty — `NotDelivered` from a failed transport,
        // `Unsettled` from a child that started and did not settle — leaves the
        // effect unobserved, because an exit code cannot distinguish "the
        // request never arrived" from "the request was applied and the answer
        // was lost".
        //
        // `NotDelivered` is the subtle one, and reading it as "nothing
        // happened" is the exact defect this classifier exists to prevent: a
        // client that applied a write and then lost its connection exits
        // non-zero, and skipping the reconciliation read on that basis loses
        // the one case the whole lost-response path is for.
        FlowError::Bot { ref source, .. } => !matches!(
            **source,
            crate::error::BotError::DomainError {
                certainty: crate::error::DispatchCertainty::Refused,
                ..
            }
        ),
        // A stop or a deadline during the publish step: the child may have been
        // written and the response dropped, so this is unknown, not "not done".
        FlowError::TimedOut { .. } | FlowError::Cancelled { .. } => true,
        // A validation failure before the write — an unusable commit id, an
        // event GitHub does not accept, a malformed event — established that
        // nothing was sent, so a retry is a retry rather than a duplicate.
        _ => false,
    }
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
