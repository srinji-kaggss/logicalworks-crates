//! What a call sends and what it returns: the pinned pull request, the review payload, the transport outcome, and the read-back ceilings.

use lgwks_std::json::{self, Serialize};

use super::{CommitId, GhError, Repository, ReviewComment};

/// The pull request whose head is pinned for one review.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PullRequest {
    /// The repository the pull request lives in.
    repository: Repository,
    /// The pull request number.
    number: u64,
}

impl PullRequest {
    /// Which repository, as owner and name, the pull request number below belongs to.
    #[must_use]
    pub const fn repository(&self) -> &Repository {
        &self.repository
    }

    /// The pull request number.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }
}

impl PullRequest {
    /// Name a pull request to review.
    #[must_use]
    pub fn new(repository: Repository, number: u64) -> Self {
        Self { repository, number }
    }
}

/// What one review should say, bound to the commit it is about.
///
/// The subject commit is a field rather than an argument to the publish call
/// because it is the property that makes a publication correct: a review of a
/// pull request whose head moved is not a review of the new head, and the only
/// way to guarantee that is to carry the reviewed commit in the payload and let
/// the transport refuse a payload that names none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(crate = "lgwks_std::json::serde")]
#[non_exhaustive]
pub struct ReviewPayload {
    /// The commit this review is about. Sent as `commit_id`, never omitted.
    pub(super) commit_id: String,
    /// The review event: `COMMENT`, `APPROVE` or `REQUEST_CHANGES`.
    pub(super) event: String,
    /// The top-level body, with the marker trailer when there is a marker.
    pub(super) body: String,
    /// The inline comments to publish, in order.
    ///
    /// Each is a distinct remote operation from the top-level body (PR-08), so
    /// the create request carries them as a `comments` array rather than folding
    /// them into the body. Omitted from the wire when empty, so a payload that
    /// declares no comments is byte-for-byte what it always was.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) comments: Vec<ReviewComment>,
    /// An application marker that helps a read-back locate a candidate review.
    ///
    /// GitHub's create-review request has no field for it, so it is not sent
    /// as one: [`ReviewPayload::new`] appends it to the body as an HTML comment
    /// (invisible when GitHub renders the review), which is the only place a
    /// read-back can find it again. A locator, never a proof on its own:
    /// [`ReviewRecord::matches`] verifies subject, the whole body and state
    /// before a publication is reported.
    #[serde(skip)]
    pub(super) marker: String,
}

impl ReviewPayload {
    /// The commit this review is about.
    #[must_use]
    pub fn commit_id(&self) -> &str {
        &self.commit_id
    }

    /// GitHub's review verdict word that will be submitted with the payload: COMMENT, APPROVE or REQUEST_CHANGES.
    #[must_use]
    pub fn event(&self) -> &str {
        &self.event
    }

    /// The top-level body exactly as it is sent, marker trailer included.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// The review state GitHub reports for a review created with this event.
    ///
    /// The create request and the read-back spell the same fact differently:
    /// `COMMENT` reads back as `COMMENTED`, `APPROVE` as `APPROVED`, and
    /// `REQUEST_CHANGES` as `CHANGES_REQUESTED`. A verifier comparing the
    /// request's spelling against the read-back would never match.
    #[must_use]
    pub fn state(&self) -> &'static str {
        match self.event.as_str() {
            "APPROVE" => "APPROVED",
            "REQUEST_CHANGES" => "CHANGES_REQUESTED",
            // `new` admits only the three `EVENTS`, so this is `COMMENT`.
            _ => "COMMENTED",
        }
    }

    /// The hidden trailer text that a read-back searches for, so a later run can locate this review again.
    #[must_use]
    pub fn marker(&self) -> &str {
        &self.marker
    }

    /// The inline comments the payload will publish.
    #[must_use]
    pub fn comments(&self) -> &[ReviewComment] {
        &self.comments
    }
}

impl ReviewPayload {
    /// The review events GitHub accepts.
    pub const EVENTS: [&'static str; 3] = ["COMMENT", "APPROVE", "REQUEST_CHANGES"];

    /// The most bytes a marker may hold.
    pub const MAX_MARKER_BYTES: usize = 256;

    /// Build a payload bound to `subject`.
    ///
    /// A non-empty `marker` is appended to `body` as
    /// `<!-- lgwks-review:{marker} -->` after a blank line, so the review a
    /// read-back returns carries it and [`ReviewPayload::body`] is exactly what
    /// GitHub will hold.
    ///
    /// # Errors
    ///
    /// [`GhError::Event`] naming the events that are accepted, when `event` is
    /// not one of them, and [`GhError::Marker`] when `marker` could close or
    /// break out of the HTML comment that carries it, or is longer than
    /// [`ReviewPayload::MAX_MARKER_BYTES`].
    pub fn new(
        subject: &CommitId,
        event: impl Into<String>,
        body: impl Into<String>,
        marker: impl Into<String>,
    ) -> Result<Self, GhError> {
        let event = event.into();
        if !Self::EVENTS.contains(&event.as_str()) {
            let refusal = Err(GhError::Event { event });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        let marker = marker.into();
        let invalid = |reason: &'static str| GhError::Marker {
            marker: marker.clone(),
            reason,
        };
        if marker.len() > Self::MAX_MARKER_BYTES {
            let refusal = Err(invalid("a marker is at most 256 bytes"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if marker.contains("--") || marker.contains(['<', '>']) || marker.contains(char::is_control)
        {
            let refusal = Err(invalid(
                "a marker travels inside an HTML comment, so it may not hold `--`, `<`, `>` or a control character",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        let mut body = body.into();
        if !marker.is_empty() {
            body.push_str("\n\n<!-- lgwks-review:");
            body.push_str(&marker);
            body.push_str(" -->");
        }
        Ok(Self {
            commit_id: subject.as_str().to_owned(),
            event,
            body,
            comments: Vec::new(),
            marker,
        })
    }

    /// Attach `comments` to the payload, replacing any already carried.
    ///
    /// Each comment is a distinct remote operation from the top-level body, so
    /// a review whose comments were accepted only in part is a *partial*
    /// submission rather than the whole one; the reconciliation compares the
    /// count that landed against the count declared here.
    #[must_use]
    pub fn with_comments(mut self, comments: Vec<ReviewComment>) -> Self {
        self.comments = comments;
        self
    }
}

/// What one call to the GitHub adapter observed.
///
/// `#[non_exhaustive]`: fields grow with what a call can report, and a
/// consumer that destructured this literally would break on each addition.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct GhOutcome {
    /// The exit code the child reported, or `None` when the supervisor stopped
    /// it before it exited on its own.
    pub(super) exit_code: Option<i32>,
    /// The retained stdout, at most the capture ceiling.
    pub(super) stdout: String,
    /// The retained stderr, at most the capture ceiling.
    pub(super) stderr: String,
    /// Whether stdout held more than the retained ceiling.
    pub(super) stdout_truncated: bool,
    /// Whether stderr held more than the retained ceiling.
    pub(super) stderr_truncated: bool,
    /// Every byte the child wrote to stdout, retained or not.
    pub(super) stdout_total_bytes: u64,
    /// Every byte the child wrote to stderr, retained or not.
    pub(super) stderr_total_bytes: u64,
    /// Whether the supervisor stopped the child because its deadline elapsed.
    pub(super) deadline_fired: bool,
    /// Whether the process group's cleanup was confirmed absent.
    pub(super) cleanup_confirmed: bool,
}

impl GhOutcome {
    /// The exit code, when the child exited on its own.
    #[must_use]
    pub const fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// The retained stdout, at most the capture ceiling.
    #[must_use]
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    /// The retained stderr, at most the capture ceiling.
    #[must_use]
    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    /// Whether stdout held more than the retained ceiling.
    #[must_use]
    pub const fn stdout_truncated(&self) -> bool {
        self.stdout_truncated
    }

    /// Whether stderr held more than the retained ceiling.
    #[must_use]
    pub const fn stderr_truncated(&self) -> bool {
        self.stderr_truncated
    }

    /// Every byte the child wrote to stdout, retained or not.
    #[must_use]
    pub const fn stdout_total_bytes(&self) -> u64 {
        self.stdout_total_bytes
    }

    /// Every byte the child wrote to stderr, retained or not.
    #[must_use]
    pub const fn stderr_total_bytes(&self) -> u64 {
        self.stderr_total_bytes
    }

    /// Whether the supervisor stopped the child because its deadline elapsed.
    #[must_use]
    pub const fn deadline_fired(&self) -> bool {
        self.deadline_fired
    }

    /// Whether the process group was confirmed absent after the run.
    #[must_use]
    pub const fn cleanup_confirmed(&self) -> bool {
        self.cleanup_confirmed
    }

    /// The HTTP status `gh` named on stderr, when it named one.
    ///
    /// `gh api` reports an API error as `gh: <message> (HTTP <status>)` on the
    /// child's stderr and exits non-zero. The status is a fact about what the
    /// server answered — `401` and `403` are a credential that cannot reach the
    /// resource, `404` is a resource that is absent **or** not visible to this
    /// credential, `406` is a diff the server declines to render — and it makes
    /// failures a caller must act on differently decidable by type rather than
    /// by reading the message. A run that named no status returns `None`, so an
    /// unclassifiable failure stays a transport failure rather than being
    /// guessed into a permission one.
    #[must_use]
    pub fn http_status(&self) -> Option<u16> {
        http_status_in(&self.stderr)
    }
}

/// The HTTP status a `gh api` error line names, when it names one.
///
/// Scans for the `HTTP ` marker `gh` writes (for example `(HTTP 404)`) and reads
/// the run of digits that follows it. A status larger than a `u16` cannot be a
/// real HTTP status, so anything that does not parse is `None`.
fn http_status_in(text: &str) -> Option<u16> {
    let (_, rest) = text.rsplit_once("HTTP ")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<u16>().ok()
}

impl GhOutcome {
    /// Whether the child exited zero on its own.
    ///
    /// An exit code is transport evidence. Whether the work the command was
    /// asked to do happened is a separate question with a separate answer, and
    /// this one only says the program said "yes".
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && !self.deadline_fired
    }

    /// Parse the retained stdout as `T`.
    ///
    /// # Errors
    ///
    /// [`GhError::MalformedResponse`] naming the path and the decode failure,
    /// with the retained text attached. A truncated stream is reported as
    /// truncated rather than decoded into a partial value.
    pub fn parse_json<T: json::serde::de::DeserializeOwned>(&self) -> Result<T, GhError> {
        if self.stdout_truncated {
            let refusal = Err(GhError::TruncatedResponse {
                retained: self.stdout.len(),
                total: self.stdout_total_bytes,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_json: returning an error to the caller");
            return refusal;
        }
        json::from_str::<T>(self.stdout.trim()).map_err(|source| GhError::MalformedResponse {
            path: String::from("stdout"),
            source: source.to_string(),
        })
    }
}

/// The most reviews one read-back will accept.
///
/// This is the ceiling that bounds `--paginate`. GitHub's reviews endpoint is
/// paginated and `gh api --paginate` follows every page, so without a stated
/// ceiling the adapter's answer grows with a pull request's history rather than
/// with its own declaration — an unbounded read dressed as a bounded one,
/// because the capture ceiling bounds the *bytes*, not the number of records a
/// verification then walks.
///
/// The number is high enough that no realistic pull request reaches it and low
/// enough that the decode is cheap: a pull request past this many reviews has
/// already had its head reviewed and superseded many times over, and the honest
/// answer for a caller that cannot hold them all is a refusal rather than a
/// prefix of the list presented as the list.
pub const MAX_REVIEWS_PER_PULL: usize = 1_000;

/// The most changed files one diff inventory will accept.
///
/// The files endpoint is paginated exactly as the reviews endpoint is, so
/// without a stated ceiling the inventory grows with a pull request's size
/// rather than with this crate's declaration. The limit is GitHub's own
/// per-comparison file ceiling, so a pull request this adapter refuses is one
/// the API itself would refuse to render: the honest answer for a caller that
/// cannot hold them all is a refusal rather than an inventory prefix presented
/// as the whole diff.
pub const MAX_DIFF_FILES_PER_PULL: usize = 3_000;

/// The most bytes of changed-file patch text one diff inventory will accept.
///
/// A separate axis from [`MAX_DIFF_FILES_PER_PULL`]: one enormous file and many
/// tiny ones grow different bounds, and a count ceiling cannot see patch text
/// growth. A list past this is refused with [`GhError::DiffTooLarge`] rather than
/// decoded into a partial diff a coverage decision could read as complete.
pub const MAX_DIFF_BYTES: usize = 256 * 1024;
