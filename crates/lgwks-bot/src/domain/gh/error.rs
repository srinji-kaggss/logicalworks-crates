//! Why a gh call refused or failed, and how each failure maps onto the crate's errors.

use super::ReviewPayload;
use crate::cap::Cap;
use crate::error::BotError;

// ── Errors ──────────────────────────────────────────────────────────────────

/// Why one GitHub adapter call could not produce what it was asked for.
///
/// The variants draw the distinctions a caller's next move turns on: a client
/// that never ran, a client that ran and failed, an answer that was not the
/// shape expected, and an answer that was truncated. None of them is a claim
/// about whether an effect landed — that is
/// [`Gh::read_reviews`](super::Gh::read_reviews)'s question.
#[derive(Debug)]
#[non_exhaustive]
pub enum GhError {
    /// The repository reference did not validate.
    Repository {
        /// The reference as given.
        spec: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The commit id did not validate.
    CommitId {
        /// The id as given.
        hex: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The review event is not one GitHub accepts.
    Event {
        /// The event as given.
        event: String,
    },
    /// The reconciliation marker cannot be carried in a review body.
    Marker {
        /// The marker as given.
        marker: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// An issue draft GitHub would refuse, refused before any call.
    IssueDraft {
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The client could not be run, or ran and could not be settled.
    #[cfg(feature = "process")]
    Process(crate::rt::process::ProcessRunError),
    /// Built without the `process` feature, so there is no supervised runner
    /// to bind the GitHub client to.
    ///
    /// A refusal rather than an empty answer on purpose: a caller handed "no
    /// reviews" would conclude GitHub has none, which is a different and
    /// entirely wrong fact.
    NoRunner,
    /// The client ran and reported failure.
    Transport {
        /// What the call was doing.
        what: String,
        /// The exit code, or `None` when there was no normal exit.
        exit_code: Option<i32>,
        /// The retained stderr.
        stderr: String,
    },
    /// The supervisor stopped the client before it answered.
    Deadline {
        /// What the call was doing.
        what: String,
    },
    /// The answer was not the shape expected.
    Response {
        /// The endpoint or path it came from.
        path: String,
        /// What was wrong with it.
        reason: String,
    },
    /// The answer was larger than the capture ceiling and was not decoded.
    TruncatedResponse {
        /// Bytes retained.
        retained: usize,
        /// Bytes the child wrote.
        total: u64,
    },
    /// The pull request holds more reviews than the adapter will decode.
    ///
    /// Reported rather than truncated. A prefix of the review list presented as
    /// the list is indistinguishable from a complete answer, and a verification
    /// over it would report "no matching review" for a review that exists on a
    /// page nobody read — the one failure mode a bounded capture cannot catch,
    /// because the truncation would have happened inside a client that decoded
    /// cleanly.
    ReviewCeiling {
        /// The endpoint the answer came from.
        path: String,
        /// How many reviews the client reported.
        reviews: usize,
        /// The ceiling that refused them.
        ceiling: usize,
    },
    /// The client reported an authentication or authorization failure.
    ///
    /// `401` and `403` are a credential that cannot reach the resource; `404`
    /// is a resource that is absent **or** not visible to this credential, and
    /// the client cannot distinguish them. All three are reported here rather
    /// than flattened into [`GhError::Transport`], because a caller must act on
    /// a permission outcome differently: a lost read permission is an
    /// *unverified* publication, never a clean failure and never a second write.
    Unauthorized {
        /// What the call was doing.
        what: String,
        /// The status the client named.
        status: u16,
        /// The retained stderr.
        reason: String,
    },
    /// GitHub rejected a write as a request (`400`, `410` or `422`), so it
    /// landed nothing.
    ///
    /// Its own variant rather than a [`GhError::Transport`]: a transport
    /// failure after a write started may have landed it and is read back,
    /// while a rejection is GitHub's answer that it did not, and reading back
    /// after one would only spend a settle finding nothing.
    Rejected {
        /// What the call was doing.
        what: String,
        /// The status the client named.
        status: u16,
        /// The retained stderr.
        reason: String,
    },
    /// The repository's answer was a moved-repository redirect.
    ///
    /// A renamed repository answers a request for its old name with a move
    /// object carrying the canonical location. The subject identity is reported
    /// rather than silently re-pointed: the review is about the repository the
    /// caller named, and a client that followed the redirect would review code
    /// nobody asked about.
    MovedRepository {
        /// The repository the caller named.
        requested: String,
        /// The canonical repository the answer named.
        canonical: String,
    },
    /// The diff for the pull request could not be produced.
    ///
    /// The server declined to render it (typically `406` for a diff too large).
    /// This is an *incomplete coverage*: a review of it would be a clean report
    /// over a diff nobody read.
    DiffUnavailable {
        /// The endpoint the answer came from.
        path: String,
        /// What the server said.
        reason: String,
    },
    /// The changed-file inventory is longer than the declared ceiling.
    ///
    /// Reported rather than truncated, for the same reason
    /// [`GhError::ReviewCeiling`] is: a prefix of the inventory that decoded
    /// cleanly is indistinguishable from the whole diff.
    DiffFileCeiling {
        /// The endpoint the answer came from.
        path: String,
        /// How many files the client reported.
        files: usize,
        /// The ceiling that refused them.
        ceiling: usize,
    },
    /// The changed files' patch text is larger than the declared ceiling.
    ///
    /// A *separate* axis from [`GhError::DiffFileCeiling`]: one enormous file
    /// and many tiny ones grow different bounds.
    DiffTooLarge {
        /// The endpoint the answer came from.
        path: String,
        /// How many bytes of patch text the client reported.
        bytes: usize,
        /// The ceiling that refused them.
        ceiling: usize,
    },
    /// The answer was not valid JSON of the expected type.
    MalformedResponse {
        /// Which stream it came from.
        path: String,
        /// The decode failure.
        source: String,
    },
    /// The payload could not be serialized, so nothing was sent.
    PayloadNotSent {
        /// Why.
        reason: String,
    },
    /// The staging file for a payload could not be written.
    Staging {
        /// The path.
        path: String,
        /// The operating system's refusal.
        source: String,
    },
}

/// The capabilities a GitHub credential stands for: the network authority the
/// token is presented over. One definition, so the need [`GhError::repair`]
/// reports and the rejection a verb raises cannot name different re-grants.
fn credential_caps() -> [Cap; 1] {
    [Cap::net()]
}

impl GhError {
    /// The repair this failure calls for, as a [`NeedSet`].
    ///
    /// `Some` for exactly the arm that is a credential problem —
    /// [`GhError::Unauthorized`], which is where the client reported `401`,
    /// `403` or a `404` the credential cannot see past — and `None` for every
    /// other failure, because none of them is repaired by authority.
    ///
    /// This is the wiring that makes an upstream auth failure a *typed repair*
    /// rather than a generic error: `401` arrives as a typed need set naming the
    /// capabilities to re-grant, so a caller acts on it once instead of
    /// discovering it from a status code and rebuilding the answer by hand. The
    /// repair is never a retry — the same credential against the same receiver
    /// gives the same status.
    ///
    /// [`NeedSet`]: crate::spec::NeedSet
    #[must_use]
    pub fn repair(&self) -> Option<crate::spec::NeedSet> {
        match *self {
            Self::Unauthorized { .. } => Some(crate::spec::NeedSet::expired_credentials(
                "gh",
                &credential_caps(),
            )),
            _ => None,
        }
    }

    /// This failure as the typed credential rejection a verb or a flow reports,
    /// attributed to `domain`, when it is one.
    ///
    /// The one place the verb path ([`PrSnapshotSource`]) and the flow path
    /// (`From<GhError> for FlowError`) turn a `401`/`403`/`404` into a
    /// [`BotError::CredentialRejected`] carrying its repair, so neither can
    /// flatten a permission loss into a generic domain failure that a retry
    /// policy would then repeat.
    pub(super) fn credential_rejection(&self, domain: &str) -> Option<BotError> {
        match *self {
            Self::Unauthorized { status, .. } => Some(crate::cap::upstream_credential_rejection(
                domain,
                status,
                &credential_caps(),
            )),
            _ => None,
        }
    }

    /// Whether repeating this call could produce a different answer.
    ///
    /// The adapter does not decide that — a caller that is about to *publish*
    /// must reconcile rather than repeat — but a read-only probe can ask, and
    /// this is the honest answer for each kind of failure.
    ///
    /// [`GhError::ReviewCeiling`] is retryable and would stay wrong on a second
    /// attempt: the pull request still holds the reviews it held. What makes a
    /// read different next time is the subject changing, not the read being
    /// repeated, so a caller that loops on this answer loops forever. It is
    /// reported here precisely so a caller can choose a ceiling rather than be
    /// handed a truncated list to mistake for the whole history.
    #[must_use]
    pub const fn is_read_only_retryable(&self) -> bool {
        matches!(
            *self,
            Self::Deadline { .. }
                | Self::MalformedResponse { .. }
                | Self::Transport { .. }
                | Self::ReviewCeiling { .. }
        )
    }

    /// Whether this refusal means the subject's coverage could not be completed.
    ///
    /// An unavailable diff and either diff ceiling are the *coverage* refusals:
    /// the pull request's changes could not be read whole, so a review built on
    /// what was read would claim a scope nobody covered. A caller distinguishes
    /// them from a transport failure because the honest response is a coverage
    /// decision — an incomplete review — rather than a run failure.
    #[must_use]
    pub const fn is_coverage_incomplete(&self) -> bool {
        matches!(
            *self,
            Self::DiffUnavailable { .. } | Self::DiffFileCeiling { .. } | Self::DiffTooLarge { .. }
        )
    }
}

impl std::fmt::Display for GhError {
    /// What was refused, and what the caller can do about it.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Repository { ref spec, reason } => {
                write!(formatter, "{spec:?}: not a repository reference: {reason}")
            }
            Self::CommitId { ref hex, reason } => {
                write!(formatter, "{hex:?}: not a commit id: {reason}")
            }
            Self::Event { ref event } => write!(
                formatter,
                "{event:?}: not a review event; expected one of {}",
                ReviewPayload::EVENTS.join(", ")
            ),
            Self::Marker { ref marker, reason } => {
                write!(formatter, "{marker:?}: not a review marker: {reason}")
            }
            Self::IssueDraft { reason } => {
                write!(formatter, "the issue draft was not sent: {reason}")
            }
            #[cfg(feature = "process")]
            Self::Process(ref source) => {
                write!(formatter, "the GitHub client could not run: {source}")
            }
            Self::NoRunner => formatter.write_str(
                "this build has no supervised process runner, so the GitHub client cannot run; \
                 rebuild with the `process` feature",
            ),
            Self::Transport {
                ref what,
                exit_code,
                ref stderr,
            } => {
                let status = match exit_code {
                    Some(code) => format!("exit {code}"),
                    None => String::from("no exit code"),
                };
                write!(formatter, "{what} failed ({status}): {stderr:?}")
            }
            Self::Deadline { ref what } => {
                write!(
                    formatter,
                    "{what} was stopped by its deadline before it answered"
                )
            }
            Self::Response {
                ref path,
                ref reason,
            } => write!(formatter, "{path}: {reason}"),
            Self::TruncatedResponse { retained, total } => write!(
                formatter,
                "the response was truncated at the capture ceiling: {retained} of {total} bytes \
                 retained, so it was not decoded"
            ),
            Self::ReviewCeiling {
                ref path,
                reviews,
                ceiling,
            } => write!(
                formatter,
                "{path}: the pull request holds {reviews} reviews, past the ceiling of \
                 {ceiling}; the list was not returned, because a prefix of it is not \
                 the review history"
            ),
            Self::MalformedResponse {
                ref path,
                ref source,
            } => {
                write!(formatter, "{path}: not the expected JSON: {source}")
            }
            Self::PayloadNotSent { ref reason } => {
                write!(formatter, "the review payload was not sent: {reason}")
            }
            Self::Staging {
                ref path,
                ref source,
            } => {
                write!(
                    formatter,
                    "{path}: the payload could not be staged: {source}"
                )
            }
            Self::Unauthorized {
                ref what,
                status,
                ref reason,
            } => write!(
                formatter,
                "{what} was refused by a credential that cannot reach the resource \
                 (HTTP {status}): {reason:?}"
            ),
            Self::Rejected {
                ref what,
                status,
                ref reason,
            } => write!(
                formatter,
                "{what} was rejected by GitHub (HTTP {status}), so nothing landed: {reason:?}"
            ),
            Self::MovedRepository {
                ref requested,
                ref canonical,
            } => write!(
                formatter,
                "the repository {requested} moved to {canonical}; the subject identity is not \
                 re-pointed, so nothing is reviewed under the canonical name until the caller \
                 names it"
            ),
            Self::DiffUnavailable {
                ref path,
                ref reason,
            } => write!(
                formatter,
                "{path}: the diff could not be produced, so the coverage is incomplete: {reason:?}"
            ),
            Self::DiffFileCeiling {
                ref path,
                files,
                ceiling,
            } => write!(
                formatter,
                "{path}: the pull request changes {files} files, past the ceiling of {ceiling}; \
                 the inventory was not returned, because a prefix of it is not the diff"
            ),
            Self::DiffTooLarge {
                ref path,
                bytes,
                ceiling,
            } => write!(
                formatter,
                "{path}: the changed files carry {bytes} bytes of patch text, past the ceiling \
                 of {ceiling}; the inventory was not returned, because a prefix of it is not \
                 the diff"
            ),
        }
    }
}

impl std::error::Error for GhError {
    /// The operating system's refusal, when there is one.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            #[cfg(feature = "process")]
            Self::Process(ref source) => Some(source),
            Self::NoRunner
            | Self::Repository { .. }
            | Self::CommitId { .. }
            | Self::Event { .. }
            | Self::Marker { .. }
            | Self::IssueDraft { .. }
            | Self::Transport { .. }
            | Self::Deadline { .. }
            | Self::Response { .. }
            | Self::TruncatedResponse { .. }
            | Self::ReviewCeiling { .. }
            | Self::Unauthorized { .. }
            | Self::Rejected { .. }
            | Self::MovedRepository { .. }
            | Self::DiffUnavailable { .. }
            | Self::DiffFileCeiling { .. }
            | Self::DiffTooLarge { .. }
            | Self::MalformedResponse { .. }
            | Self::PayloadNotSent { .. }
            | Self::Staging { .. } => None,
        }
    }
}

#[cfg(feature = "script")]
impl From<GhError> for crate::script::FlowError {
    /// A GitHub adapter failure inside a flow is a located failure carrying the
    /// adapter's own typed vocabulary as its cause.
    ///
    /// The certainty is preserved rather than flattened: an `EffectIndeterminate`
    /// stays one, so a flow's retry decision still knows that the publication
    /// may have happened. This is the same mapping the verb path makes.
    fn from(source: GhError) -> Self {
        crate::script::FlowError::Bot {
            at: std::sync::Arc::from(""),
            source: Box::new(source.into_bot_error("gh")),
        }
    }
}

#[cfg(any(feature = "script", feature = "process"))]
impl GhError {
    /// This failure as the crate error a verb returns, attributed to `domain`.
    ///
    /// One mapping for the flow path and the verb path, so the certainty a
    /// retry decision reads is the same however the adapter was reached.
    pub(super) fn into_bot_error(self, domain: &str) -> BotError {
        // A refused credential is its own typed outcome, with the repair
        // attached, rather than a `DomainError` a caller would retry.
        if let Some(rejection) = self.credential_rejection(domain) {
            return rejection;
        }
        let certainty = match self {
            // The child started and did not settle: it may have reached
            // GitHub, so the effect is unsettled rather than undelivered. A
            // flow's retry policy reads that distinction, and a retry of an
            // unsettled publication is the duplicate this crate refuses to make.
            #[cfg(feature = "process")]
            GhError::Process(crate::rt::process::ProcessRunError::AfterStart { .. }) => {
                crate::error::DispatchCertainty::Unsettled
            }
            // Refused before any child existed: an invalid subject, payload or
            // marker, a payload that could not be staged, a build with no
            // runner, or a client the platform would not start. Nothing can
            // have reached GitHub, so this is `Refused` — the one certainty a
            // publisher may act on without reading back first.
            #[cfg(feature = "process")]
            GhError::Process(
                crate::rt::process::ProcessRunError::Refused
                | crate::rt::process::ProcessRunError::NotStarted { .. },
            ) => crate::error::DispatchCertainty::Refused,
            GhError::Repository { .. }
            | GhError::CommitId { .. }
            | GhError::Event { .. }
            | GhError::Marker { .. }
            | GhError::IssueDraft { .. }
            | GhError::NoRunner
            | GhError::PayloadNotSent { .. }
            | GhError::Staging { .. }
            // GitHub answered that the request itself is unacceptable: the
            // child ran, and the write still landed nothing.
            | GhError::Rejected { .. } => crate::error::DispatchCertainty::Refused,
            // The client ran: whatever it was asked to do may have happened
            // and its answer been lost or misread.
            _ => crate::error::DispatchCertainty::NotDelivered,
        };
        match certainty {
            crate::error::DispatchCertainty::Unsettled => BotError::EffectIndeterminate {
                domain: domain.to_owned(),
                cause: self.to_string(),
            },
            _ => BotError::DomainError {
                domain: domain.to_owned(),
                certainty,
                cause: self.to_string(),
            },
        }
    }
}
