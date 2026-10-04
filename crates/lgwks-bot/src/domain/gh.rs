//! `gh` owns the GitHub domain through the `gh` command-line client.
//!
//! Every call is one supervised child process: the GitHub CLI is the only
//! handle this crate has on GitHub, so it is admitted as a `ProcessSpec` and
//! run through the supervisor, which owns the process group, the bounded
//! capture and the deadline. Nothing
//! here spawns a process, manages a pid, or parses a shell string: arguments
//! are passed as a vector, so a repository name, a body or a commit id is never
//! re-parsed by a shell.
//!
//! The three facts this module is careful about:
//!
//! - **Transport evidence is not publication.** A non-zero exit says the child
//!   failed; it says nothing about whether a review landed. Publication is
//!   established by [`Gh::read_reviews`], which is a separate call.
//! - **Capability is checked before the fork.** Every verb takes an [`Auth`]
//!   covering `bot.sys` (process control, which is what running `gh` is) and
//!   `bot.net` (which is what the CLI reaches), and refuses without starting
//!   anything.
//! - **Bounded output.** Each call retains at most its declared capture
//!   ceiling of stdout and counts every byte the child wrote, so a chatty or
//!   hostile `gh` is truncated with a report rather than retained without
//!   bound. Byte bounds and *count* bounds are different, and the second is
//!   [`MAX_REVIEWS_PER_PULL`]: a read-back that decoded cleanly but stopped at
//!   the end of what a paginating client happened to print looks exactly like a
//!   complete answer, so a list past the ceiling is refused rather than
//!   returned short.
//!
//! Without the `process` feature there is no supervised runner to bind to, so
//! every call refuses with [`GhError::NoRunner`]: a GitHub binding that cannot
//! run a client says so rather than pretending it read GitHub.

#[cfg(feature = "process")]
use std::num::NonZeroUsize;
#[cfg(feature = "process")]
use std::time::Duration;

use lgwks_std::json::{self, Deserialize, Serialize};

use crate::cap::{Auth, Cap};
use crate::error::{BotError, DispatchCertainty};
use crate::verb;

#[cfg(feature = "process")]
use super::sys::{DEFAULT_CAPTURE_LIMIT, DEFAULT_DEADLINE};

#[cfg(feature = "process")]
use crate::rt::process::{ProcessRun, ProcessSpec};
#[cfg(feature = "process")]
use crate::rt::supervise::Supervisor;

// ── GitHub identifiers ──────────────────────────────────────────────────────

/// The owner half of an `owner/repo` repository reference.
///
/// A newtype rather than a `String`, because a review's subject is three
/// things at once — repository, number, and commit — and the only way a
/// publication can be wrong is by being about the wrong one of them. Typing the
/// repository keeps a bare string from being passed where a commit is meant.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Repository {
    /// `owner/repo`, validated below.
    spec: String,
}

impl Repository {
    /// Validate and hold an `owner/repo` reference.
    ///
    /// # Errors
    ///
    /// [`GhError::Repository`] naming what is wrong with `spec`.
    pub fn new(spec: impl Into<String>) -> Result<Self, GhError> {
        let spec = spec.into();
        let invalid = |reason: &'static str| GhError::Repository {
            spec: spec.clone(),
            reason,
        };
        if spec.is_empty() {
            let refusal = Err(invalid("the repository reference is empty"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if spec.len() > MAX_REPOSITORY_BYTES {
            let refusal = Err(invalid("the repository reference is too long"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        let mut parts = spec.split('/');
        let owner = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        if parts.next().is_some() {
            let refusal = Err(invalid(
                "a repository reference is `owner/repo`, with exactly one `/`",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if owner.is_empty() || name.is_empty() {
            let refusal = Err(invalid("both `owner` and `repo` must be non-empty"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        // `.` and `..` are path segments, not names: `../..` would turn
        // `repos/{owner}/{repo}/pulls` into a request for another endpoint.
        if [owner, name]
            .iter()
            .any(|segment| matches!(*segment, "." | ".."))
        {
            let refusal = Err(invalid(
                "`.` and `..` are path segments, not repository names",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        let allowed = |segment: &str| {
            segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        };
        if !allowed(owner) || !allowed(name) {
            let refusal = Err(invalid(
                "`owner` and `repo` hold ASCII letters, digits, '-', '_' and '.' only",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        Ok(Self { spec })
    }

    /// The validated `owner/repo` reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.spec
    }
}

impl std::fmt::Display for Repository {
    /// The validated `owner/repo` reference.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.spec)
    }
}

/// A 40-character lowercase hexadecimal commit id.
///
/// Length and alphabet are checked, so a truncated or mistyped sha is refused
/// before it is sent as a subject rather than becoming a server-side 404 that
/// looks like a missing review.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommitId(String);

impl CommitId {
    /// Validate and hold a commit id.
    ///
    /// # Errors
    ///
    /// [`GhError::CommitId`] naming what is wrong with `hex`.
    pub fn new(hex: impl Into<String>) -> Result<Self, GhError> {
        let hex = hex.into();
        if hex.len() != SHA_HEX_LEN {
            let refusal = Err(GhError::CommitId {
                hex,
                reason: "a commit id is 40 hexadecimal characters",
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            let refusal = Err(GhError::CommitId {
                hex,
                reason: "a commit id is lowercase hexadecimal",
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        Ok(Self(hex))
    }

    /// The validated hexadecimal commit id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CommitId {
    /// The validated hexadecimal commit id.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The length of a full commit id in hexadecimal characters.
pub const SHA_HEX_LEN: usize = 40;

/// The most bytes an `owner/repo` reference may hold.
pub const MAX_REPOSITORY_BYTES: usize = 256;

// ── The read-back record ────────────────────────────────────────────────────

/// One review as GitHub reports it, which is what a publication is verified
/// against.
///
/// This is a *transport* type on purpose. It carries only the fields the
/// verification reads — id, subject commit, state, body, author — because a
/// verified publication is a claim about those and nothing wider. The body is
/// optional so a review GitHub returns with no body compares equal to an
/// intended empty body rather than to a missing field.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(crate = "lgwks_std::json::serde")]
#[non_exhaustive]
pub struct ReviewRecord {
    /// GitHub's numeric review id.
    #[serde(default)]
    id: u64,
    /// The commit the review is about, when GitHub reports one.
    #[serde(default)]
    commit_id: Option<String>,
    /// `PENDING`, `COMMENTED`, `APPROVED`, `CHANGES_REQUESTED` or `DISMISSED`.
    #[serde(default)]
    state: String,
    /// The top-level body.
    #[serde(default)]
    body: Option<String>,
    /// How many inline comments the receiver reports on this review.
    ///
    /// `None` is a fact about the answer, not a missing field: a receiver that
    /// reports no comment count has not established that any comment landed,
    /// and a verifier that read that as "all of them did" would verify a
    /// partial submission as a whole one. The review object the GitHub REST API
    /// returns for the reviews endpoint does not enumerate comments, so the
    /// count is what a read-back can compare without a second call per review.
    #[serde(default)]
    comment_count: Option<u64>,
    /// The login of the review's author.
    #[serde(default)]
    user: Option<ReviewAuthor>,
}

impl ReviewRecord {
    /// GitHub's numeric review id, which is what a publication is named by.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// The commit this review is about, when GitHub reports one.
    ///
    /// `None` is a fact about the answer, not a missing field: GitHub reports
    /// no commit id for some review states, and a verifier that treated that
    /// as "the subject matched" would verify nothing.
    #[must_use]
    pub fn commit_id(&self) -> Option<&str> {
        self.commit_id.as_deref()
    }

    /// The review state GitHub reports.
    #[must_use]
    pub fn state(&self) -> &str {
        &self.state
    }

    /// The top-level body, when GitHub reports one.
    #[must_use]
    pub fn body(&self) -> Option<&str> {
        self.body.as_deref()
    }

    /// The review's author, when GitHub reports one.
    #[must_use]
    pub const fn user(&self) -> Option<&ReviewAuthor> {
        self.user.as_ref()
    }

    /// The inline comment count a verification compares, with an unreported
    /// count read as zero.
    ///
    /// Zero is the honest default in the comparing direction: a review whose
    /// receiver reported no comment count has established no comment landed, so
    /// a payload that intended comments does not match it and the reconciliation
    /// reports the difference rather than assuming it away.
    #[must_use]
    pub fn applied_comments(&self) -> usize {
        self.comment_count
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0)
    }

    /// Whether the receiver reports this review as an unsubmitted draft.
    ///
    /// GitHub creates a review in `PENDING` when the create request omits an
    /// event. A pending review is an effect that landed and was never submitted,
    /// which is a different fact from a submitted review and from no review at
    /// all — and it is not a publication.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.state == REVIEW_STATE_PENDING
    }

    /// Whether this record is about the same subject and says the same body as
    /// `intended`, ignoring state and comment count.
    ///
    /// Used to recognise a *pending draft* of the intended review: the same
    /// commit and the same body, held in `PENDING` rather than submitted.
    #[must_use]
    pub fn matches_subject_body(&self, intended: &ReviewPayload) -> bool {
        self.commit_id.as_deref() == Some(intended.commit_id.as_str())
            && self.body.as_deref() == Some(intended.body.as_str())
    }

    /// Whether this record matches `intended` on subject, body and state, but
    /// not necessarily on the inline comment count.
    ///
    /// The partial-submission case: the review was submitted about the right
    /// commit saying the right thing, and fewer comments than intended landed.
    #[must_use]
    pub fn matches_except_comments(&self, intended: &ReviewPayload) -> bool {
        self.matches_subject_body(intended) && self.state == intended.state()
    }

    /// Build a record from the fields a read-back verification reads.
    ///
    /// Public because a consumer that keeps its own receipt of what it
    /// published needs to reconstruct the same record its own adapter will
    /// return, and a private field would force it to compare against a
    /// different shape. It builds only what [`ReviewRecord::matches`] reads, so
    /// it cannot be used to assert a match that was never observed.
    #[must_use]
    pub fn new(id: u64, commit_id: &str, state: &str, body: &str) -> Self {
        Self {
            id,
            commit_id: Some(String::from(commit_id)),
            state: String::from(state),
            body: Some(String::from(body)),
            comment_count: None,
            user: None,
        }
    }

    /// Whether this record is the review `intended` describes.
    ///
    /// The subject, the body and the state are all compared, because a
    /// read-back that matched only the subject would accept a review of the
    /// right commit saying something else — which is not verification of this
    /// publication. The state compared is the one GitHub *reports*
    /// ([`ReviewPayload::state`]): a review created with `COMMENT` reads back as
    /// `COMMENTED`, and comparing the request's spelling would verify nothing
    /// against the real API. The marker is never compared on its own: it
    /// travels inside the body, so it is checked only as part of the whole body,
    /// and a matching marker with a different body is a different review. The
    /// inline comment count is compared too: a review whose top-level body
    /// landed but whose comments were accepted only in part is a partial
    /// submission, not the whole one, and the review journey reports the
    /// difference rather than folding it into a match.
    #[must_use]
    pub fn matches(&self, intended: &ReviewPayload) -> bool {
        self.matches_except_comments(intended)
            && self.applied_comments() == intended.comments().len()
    }
}

/// The author half of a [`ReviewRecord`].
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(crate = "lgwks_std::json::serde")]
#[non_exhaustive]
pub struct ReviewAuthor {
    /// The author's login.
    #[serde(default)]
    login: Option<String>,
}

impl ReviewAuthor {
    /// The author's login, when GitHub reports one.
    #[must_use]
    pub fn login(&self) -> Option<&str> {
        self.login.as_deref()
    }
}

/// The review state GitHub reports for an unsubmitted draft.
///
/// A review created without an event is held in `PENDING` until it is
/// submitted. It is an effect that landed and was never submitted, which the
/// journey reports as its own state rather than as a publication or as no
/// effect at all.
pub const REVIEW_STATE_PENDING: &str = "PENDING";

/// One inline review comment, bound to a path and line in the pinned subject.
///
/// A comment is a distinct remote operation from the top-level review body
/// (PR-08), so it is carried as data rather than folded into the body: a review
/// whose body landed and whose comments were accepted only in part is a partial
/// submission, and the count is what makes "in part" decidable.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(crate = "lgwks_std::json::serde")]
#[non_exhaustive]
pub struct ReviewComment {
    /// The repository-relative path the comment is about.
    path: String,
    /// The line in the file the comment is about.
    line: u64,
    /// The comment body.
    body: String,
}

impl ReviewComment {
    /// A comment on `path` at `line` saying `body`.
    #[must_use]
    pub fn new(path: impl Into<String>, line: u64, body: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            line,
            body: body.into(),
        }
    }

    /// The repository-relative path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The line the comment is about.
    #[must_use]
    pub const fn line(&self) -> u64 {
        self.line
    }

    /// The text a reviewer wrote, as GitHub returned it, with no trimming or marker stripping applied.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// One changed file in a pull request, as the files endpoint reports it.
///
/// The inventory is what binds a review's findings to the pinned subject: a
/// finding whose location is not in this list is not in the diff that was read.
/// It is a *transport* view of one file — the fields a coverage decision reads.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(crate = "lgwks_std::json::serde")]
#[non_exhaustive]
pub struct ChangedFile {
    /// The repository-relative path.
    #[serde(default)]
    filename: String,
    /// `added`, `modified`, `removed`, `renamed` or similar.
    #[serde(default)]
    status: String,
    /// Lines added.
    #[serde(default)]
    additions: u64,
    /// Lines removed.
    #[serde(default)]
    deletions: u64,
    /// The unified diff hunk, when the receiver reports one.
    #[serde(default)]
    patch: Option<String>,
}

impl ChangedFile {
    /// The repository-relative path.
    #[must_use]
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// GitHub's own status word for this file in the diff, such as added, modified, removed or renamed, passed through verbatim.
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Lines added.
    #[must_use]
    pub const fn additions(&self) -> u64 {
        self.additions
    }

    /// Lines removed.
    #[must_use]
    pub const fn deletions(&self) -> u64 {
        self.deletions
    }

    /// The unified diff hunk, when the receiver reports one.
    #[must_use]
    pub fn patch(&self) -> Option<&str> {
        self.patch.as_deref()
    }

    /// The bytes of patch text this entry carries, zero when it carries none.
    #[must_use]
    pub fn patch_bytes(&self) -> usize {
        self.patch.as_ref().map_or(0, String::len)
    }
}

/// One pull request's changed-file inventory, bounded to the pinned subject.
///
/// The list is read as **data**: nothing here is compiled, imported, built,
/// shelled or loaded. A file named `build.rs` in this list is a changed file
/// whose patch text was read, never a build script that ran.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PullDiff {
    /// The pull request number the inventory was read for.
    number: u64,
    /// The changed files, in the order the receiver reported them.
    files: Vec<ChangedFile>,
    /// Total bytes of patch text, summed over every changed file.
    patch_bytes: usize,
}

impl PullDiff {
    /// The pull request number.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }

    /// Every file the pull request touches, in the order GitHub listed them, each with its status and line counts.
    #[must_use]
    pub fn files(&self) -> &[ChangedFile] {
        &self.files
    }

    /// How many files the pull request changed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the inventory is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Total bytes of patch text across every changed file.
    #[must_use]
    pub const fn patch_bytes(&self) -> usize {
        self.patch_bytes
    }
}

/// One pull request's pinned head, as `gh api` reports it.
///
/// `#[non_exhaustive]`: GitHub adds fields to this object, and a consumer that
/// destructured it literally would break on each addition.
///
/// Decoded from the object GitHub's REST API returns for
/// `repos/{owner}/{repo}/pulls/{number}`, where the commits are nested as
/// `head.sha` and `base.sha`. A missing commit decodes to an empty sha, which
/// [`Gh::snapshot`] refuses rather than reviewing nothing.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(crate = "lgwks_std::json::serde", from = "PullWire")]
#[non_exhaustive]
pub struct PrSnapshot {
    /// The pull request number.
    number: u64,
    /// The head commit's sha at the moment of the read.
    head_sha: String,
    /// The base commit's sha at the moment of the read.
    base_sha: String,
}

/// The pull-request object as GitHub sends it, reduced to what a snapshot reads.
#[derive(Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct PullWire {
    /// The pull request number.
    #[serde(default)]
    number: u64,
    /// `head`, whose `sha` is the commit a review is pinned to.
    #[serde(default)]
    head: RefWire,
    /// `base`, whose `sha` is the commit the head is compared against.
    #[serde(default)]
    base: RefWire,
}

/// One `head` or `base` reference in a pull-request object.
#[derive(Default, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct RefWire {
    /// The commit the reference points at.
    #[serde(default)]
    sha: String,
}

/// The body GitHub sends for a renamed repository, reduced to what it takes to
/// name the canonical repository.
///
/// A renamed repository answers a request for its old name with a
/// `Moved Permanently` object rather than a pull request. The `url` names the
/// canonical API location, from which the canonical `owner/repo` is read; a
/// client that silently followed it would review a subject the caller never
/// named, so the adapter reports the move and the journey refuses.
#[cfg(feature = "process")]
#[derive(Default, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct MovedWire {
    /// The server's message, typically `Moved Permanently`.
    #[serde(default)]
    message: Option<String>,
    /// The canonical API URL, when the answer names one.
    #[serde(default)]
    url: Option<String>,
}

#[cfg(feature = "process")]
impl MovedWire {
    /// The canonical `owner/repo` this move names, when the answer names one.
    ///
    /// Only a `Moved Permanently` message with a parseable canonical URL is a
    /// move. A body that carries neither is not a move, so it is reported as a
    /// malformed pull request rather than as a renamed repository.
    fn canonical(&self) -> Option<String> {
        let message = self.message.as_deref()?;
        if !message.to_ascii_lowercase().contains("moved") {
            return None;
        }
        canonical_repo_from_url(self.url.as_deref()?)
    }
}

/// The `owner/repo` named by an `api.github.com/repos/...` URL.
///
/// Reads the two path segments after `/repos/` and validates the result through
/// [`Repository::new`], so a URL that does not name a well-formed repository
/// yields `None` rather than a malformed identity.
#[cfg(feature = "process")]
fn canonical_repo_from_url(url: &str) -> Option<String> {
    let (_, tail) = url.split_once("/repos/")?;
    let mut segments = tail.split('/');
    let owner = segments.next()?;
    let repo = segments.next()?;
    let spec = format!("{owner}/{repo}");
    Repository::new(spec)
        .ok()
        .map(|repo| repo.as_str().to_owned())
}

impl From<PullWire> for PrSnapshot {
    /// Lift the nested commits into the snapshot's flat fields.
    fn from(wire: PullWire) -> Self {
        Self {
            number: wire.number,
            head_sha: wire.head.sha,
            base_sha: wire.base.sha,
        }
    }
}

impl PrSnapshot {
    /// The pull request number.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }

    /// The head commit's sha at the moment of the read.
    ///
    /// This is the value a publication's subject is compared against, and the
    /// only thing that makes "the head moved" decidable. A review published
    /// after a change to this value is a review of a commit nobody read.
    #[must_use]
    pub fn head_sha(&self) -> &str {
        &self.head_sha
    }

    /// The base commit's sha at the moment of the read.
    #[must_use]
    pub fn base_sha(&self) -> &str {
        &self.base_sha
    }
}

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
    commit_id: String,
    /// The review event: `COMMENT`, `APPROVE` or `REQUEST_CHANGES`.
    event: String,
    /// The top-level body, with the marker trailer when there is a marker.
    body: String,
    /// The inline comments to publish, in order.
    ///
    /// Each is a distinct remote operation from the top-level body (PR-08), so
    /// the create request carries them as a `comments` array rather than folding
    /// them into the body. Omitted from the wire when empty, so a payload that
    /// declares no comments is byte-for-byte what it always was.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    comments: Vec<ReviewComment>,
    /// An application marker that helps a read-back locate a candidate review.
    ///
    /// GitHub's create-review request has no field for it, so it is not sent
    /// as one: [`ReviewPayload::new`] appends it to the body as an HTML comment
    /// (invisible when GitHub renders the review), which is the only place a
    /// read-back can find it again. A locator, never a proof on its own:
    /// [`ReviewRecord::matches`] verifies subject, the whole body and state
    /// before a publication is reported.
    #[serde(skip)]
    marker: String,
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
    exit_code: Option<i32>,
    /// The retained stdout, at most the capture ceiling.
    stdout: String,
    /// The retained stderr, at most the capture ceiling.
    stderr: String,
    /// Whether stdout held more than the retained ceiling.
    stdout_truncated: bool,
    /// Whether stderr held more than the retained ceiling.
    stderr_truncated: bool,
    /// Every byte the child wrote to stdout, retained or not.
    stdout_total_bytes: u64,
    /// Every byte the child wrote to stderr, retained or not.
    stderr_total_bytes: u64,
    /// Whether the supervisor stopped the child because its deadline elapsed.
    deadline_fired: bool,
    /// Whether the process group's cleanup was confirmed absent.
    cleanup_confirmed: bool,
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

// ── The adapter ─────────────────────────────────────────────────────────────

/// The `gh` command-line client, as a GitHub binding.
///
/// Built with [`Gh::new`], and every call runs one supervised child. The
/// executable is resolved through `PATH` by the operating system, which is the
/// honest statement of what this adapter trusts: it does not verify the
/// client's identity, and a caller who needs that must pin a path. The
/// [`BotSpec`](crate::BotSpec) for a deployment declares that trust in the
/// adapter set rather than discovering it by running something.
#[derive(Debug, Clone)]
pub struct Gh {
    /// `owner/repo` every call targets.
    repository: Repository,
    /// The executable name or absolute path, passed straight to the platform.
    program: String,
    /// The retained-byte ceiling for each captured stream.
    #[cfg(feature = "process")]
    capture: NonZeroUsize,
    /// The maximum runtime of one call.
    #[cfg(feature = "process")]
    deadline: Option<Duration>,
    /// Environment deltas applied to every child, in order.
    #[cfg(feature = "process")]
    env: Vec<(String, String)>,
    /// Forced by the constructor; see [`Gh::required_caps`].
    caps: Vec<Cap>,
}

impl Gh {
    /// Create the `gh` binding for `repository`, resolving `gh` through `PATH`.
    ///
    /// The default capture ceiling and deadline are the same ones
    /// [`sys::Process`](super::sys::Process) applies, so no call this adapter
    /// makes is unbounded in either.
    #[must_use]
    pub fn new(repository: Repository) -> Self {
        Self {
            repository,
            program: String::from("gh"),
            #[cfg(feature = "process")]
            capture: DEFAULT_CAPTURE_LIMIT,
            #[cfg(feature = "process")]
            deadline: Some(DEFAULT_DEADLINE),
            #[cfg(feature = "process")]
            env: Vec::new(),
            caps: vec![Cap::sys(), Cap::net()],
        }
    }

    /// Run `program` instead of `gh`, resolved through `PATH` like any other.
    ///
    /// This is how a test — or a deployment that pins a wrapper — binds the
    /// adapter to a specific client without the adapter growing a notion of
    /// testing.
    #[must_use]
    pub fn program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    /// Retain at most `limit` bytes of each stream.
    ///
    /// Every byte the child writes is still counted, so a call whose answer was
    /// truncated is reported as truncated rather than as a short answer.
    #[cfg(feature = "process")]
    #[must_use]
    pub fn capture_limit(mut self, limit: NonZeroUsize) -> Self {
        self.capture = limit;
        self
    }

    /// Give each call at most `deadline` before the supervisor stops the whole
    /// process group. `None` removes the bound, which the crate does not
    /// recommend and which exists only for a caller that has a bound elsewhere.
    #[cfg(feature = "process")]
    #[must_use]
    pub fn deadline(mut self, deadline: Option<Duration>) -> Self {
        self.deadline = deadline;
        self
    }

    /// Set `key` to `value` in the environment of every call this binding makes.
    ///
    /// The environment policy belongs to the binding rather than to the process
    /// that happens to be running it: a caller that pins a client needs a
    /// pinned `PATH`, and a caller whose credentials are scoped to this one
    /// repository needs `GH_REPO` set here rather than in a shell. It is a
    /// per-child delta, so it is bounded by the number of variables the caller
    /// declared rather than by the ambient environment.
    #[cfg(feature = "process")]
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl AsRef<std::ffi::OsStr>) -> Self {
        self.env
            .push((key.into(), value.as_ref().to_string_lossy().into_owned()));
        self
    }

    /// What the constructor forced. Both capabilities, always.
    ///
    /// Public because a caller planning authority has to be able to ask, and
    /// `bot.sys` is not obvious from the word "GitHub": running a client is
    /// process control, and reaching GitHub through it is the network.
    #[must_use]
    pub fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    /// The repository every call targets.
    #[must_use]
    pub fn repository(&self) -> &Repository {
        &self.repository
    }

    /// `gh api <path>` for a repository-scoped REST path.
    #[must_use]
    pub fn api_path(&self, suffix: &str) -> String {
        format!("repos/{}{suffix}", self.repository.as_str())
    }

    /// One supervised `gh` call, or the typed failure of running it.
    #[cfg(feature = "process")]
    async fn call(&self, args: &[String]) -> Result<GhOutcome, GhError> {
        self.run_spec(args).await
    }

    /// Without the `process` feature there is no runner, and saying so is the
    /// honest answer: a binding that cannot reach GitHub must not report an
    /// empty answer, which a caller could mistake for "GitHub has no reviews".
    #[cfg(not(feature = "process"))]
    async fn call(&self, _args: &[String]) -> Result<GhOutcome, GhError> {
        Err(GhError::NoRunner)
    }

    /// The `ProcessSpec` one call runs, before it is started.
    #[cfg(feature = "process")]
    fn spec_for(&self, args: &[String]) -> ProcessSpec {
        let mut spec = ProcessSpec::new(&self.program);
        for arg in args {
            spec.arg(arg);
        }
        // `gh api` reads a request body only from a named file; a null stdin
        // makes a client that tries to prompt instead fail immediately.
        spec.stdin(crate::rt::process::StdioPolicy::Null);
        spec.capture_stdout(self.capture);
        spec.capture_stderr(self.capture);
        if let Some(deadline) = self.deadline {
            spec.deadline(deadline);
        }
        for delta in self.env.iter() {
            spec.env(&delta.0, &delta.1);
        }
        spec
    }

    /// Run one already-described call on a supervisor of its own.
    #[cfg(feature = "process")]
    async fn run_spec(&self, args: &[String]) -> Result<GhOutcome, GhError> {
        let spec = self.spec_for(args);
        let mut supervisor = Supervisor::new(1);
        match supervisor.run_process(&spec).await {
            Ok(run) => Ok(outcome_of(&run)),
            Err(source) => Err(GhError::Process(source)),
        }
    }

    /// The `gh` argument vector for a call, before it is run.
    ///
    /// Exposed so a caller — a test, or a diagnostic — can see exactly what
    /// would be executed without executing it. This is the array the platform
    /// receives; there is no shell anywhere in this path.
    #[must_use]
    pub fn args_for(&self, rest: &[&str]) -> Vec<String> {
        let mut args = vec![String::from("api")];
        args.extend(rest.iter().map(|arg| (*arg).to_owned()));
        args
    }

    /// Read the pinned head and base of `pull`, without the `process` feature.
    ///
    /// There is no supervised runner in this build, so the call is not made and
    /// the answer is not invented. A binding that cannot reach GitHub must not
    /// report a pull request with an empty head, which a caller would read as a
    /// real answer and go on to review nothing.
    ///
    /// # Errors
    ///
    /// [`GhError::NoRunner`], always. Rebuild with the `process` feature to bind
    /// this adapter to a supervised runner.
    #[cfg(not(feature = "process"))]
    pub async fn snapshot(&self, _pull: &PullRequest) -> Result<PrSnapshot, GhError> {
        Err(GhError::NoRunner)
    }

    /// Read the pinned head and base of `pull`.
    ///
    /// # Errors
    ///
    /// a client failure when it could not be run at all,
    /// [`GhError::Transport`] naming the exit code when it ran and failed,
    /// [`GhError::Deadline`] when the supervisor stopped it before it
    /// answered, and [`GhError::Response`] or [`GhError::MalformedResponse`]
    /// when it answered with something that is not a pull request.
    #[cfg(feature = "process")]
    pub async fn snapshot(&self, pull: &PullRequest) -> Result<PrSnapshot, GhError> {
        let path = format!(
            "repos/{}/pulls/{}",
            pull.repository().as_str(),
            pull.number()
        );
        let args = self.args_for(&["--method", "GET", &path]);
        let outcome = self.call(&args).await?;
        outcome.require_success("reading the pull request head")?;
        let snapshot = outcome.parse_json::<PrSnapshot>()?;
        // A renamed repository answers with a move object rather than a pull
        // request, which decodes to a snapshot with no commits. It is reported
        // as a typed move naming the canonical repository, never silently
        // followed: the subject identity a review is about is the repository the
        // caller named, and re-pointing it here would review code nobody
        // asked about.
        if snapshot.head_sha().is_empty()
            && snapshot.base_sha().is_empty()
            && let Ok(moved) = outcome.parse_json::<MovedWire>()
            && let Some(canonical) = moved.canonical()
        {
            let refusal = Err(GhError::MovedRepository {
                requested: self.repository.as_str().to_owned(),
                canonical,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "snapshot: returning an error to the caller");
            return refusal;
        }
        if snapshot.head_sha().len() != SHA_HEX_LEN || snapshot.base_sha().len() != SHA_HEX_LEN {
            let refusal = Err(GhError::Response {
                path,
                reason: String::from("the pull request has no 40-character head and base"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "snapshot: returning an error to the caller");
            return refusal;
        }
        Ok(snapshot)
    }

    /// Read every review currently on `pull`, without the `process` feature.
    ///
    /// There is no supervised runner in this build, so the call is not made and
    /// no review is read. An empty `Vec` here would be the worst possible answer
    /// for this particular call: it is exactly what a caller would read as "the
    /// pull request has no reviews", which is a fact about GitHub this build has
    /// no way to know. The refusal is the honest one.
    ///
    /// # Errors
    ///
    /// [`GhError::NoRunner`], always. Rebuild with the `process` feature to bind
    /// this adapter to a supervised runner.
    #[cfg(not(feature = "process"))]
    pub async fn read_reviews(&self, _pull: &PullRequest) -> Result<Vec<ReviewRecord>, GhError> {
        Err(GhError::NoRunner)
    }

    /// Read the changed-file inventory of `pull`, without the `process` feature.
    ///
    /// There is no supervised runner in this build, so the call is not made and
    /// no inventory is invented. An empty list here would be read as "the pull
    /// request changed no files" — a claim this build cannot make — so the
    /// refusal is the honest one, exactly as it is for every other read.
    ///
    /// # Errors
    ///
    /// [`GhError::NoRunner`], always. Rebuild with the `process` feature to bind
    /// this adapter to a supervised runner.
    #[cfg(not(feature = "process"))]
    pub async fn read_diff(&self, _pull: &PullRequest) -> Result<PullDiff, GhError> {
        Err(GhError::NoRunner)
    }

    /// Read every review currently on `pull`, through a real supervised call.
    ///
    /// `--paginate` makes the client follow GitHub's pages until it has them
    /// all, so the review list is bounded by the adapter's **review ceiling**
    /// ([`MAX_REVIEWS_PER_PULL`]) rather than by the client's patience. A pull
    /// request carrying more reviews than that is refused with
    /// [`GhError::ReviewCeiling`], which is a different statement from a short
    /// list: a truncated list that decoded cleanly is a *complete* answer as far
    /// as any consumer can tell, and a verification built on it would report
    /// "no matching review" for a review that is on a page nobody read.
    ///
    /// # Errors
    ///
    /// As [`Gh::snapshot`], plus [`GhError::Response`] when the answer is not
    /// a review list and [`GhError::ReviewCeiling`] when the pull request holds
    /// more than [`MAX_REVIEWS_PER_PULL`] reviews.
    #[cfg(feature = "process")]
    pub async fn read_reviews(&self, pull: &PullRequest) -> Result<Vec<ReviewRecord>, GhError> {
        let path = format!(
            "repos/{}/pulls/{}/reviews",
            pull.repository().as_str(),
            pull.number()
        );
        let args = self.args_for(&["--method", "GET", &path, "--paginate"]);
        let outcome = self.call(&args).await?;
        outcome.require_success("reading the pull request's reviews")?;
        let reviews = outcome.parse_json::<Vec<ReviewRecord>>()?;
        // Counted after the decode rather than while reading the pages, because
        // the decode is already bounded by the capture ceiling: this check is
        // about *completeness*, not about memory. A list over the ceiling is
        // refused outright, never truncated and returned.
        if reviews.len() > MAX_REVIEWS_PER_PULL {
            let refusal = Err(GhError::ReviewCeiling {
                path,
                reviews: reviews.len(),
                ceiling: MAX_REVIEWS_PER_PULL,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read_reviews: returning an error to the caller");
            return refusal;
        }
        Ok(reviews)
    }

    /// Read the changed-file inventory of `pull`, through a real supervised call.
    ///
    /// The files endpoint is paginated exactly as the reviews endpoint is, so
    /// the inventory is bounded on **two separate axes** rather than by the
    /// client's patience: at most [`MAX_DIFF_FILES_PER_PULL`] files
    /// ([`GhError::DiffFileCeiling`]) and at most [`MAX_DIFF_BYTES`] bytes of
    /// patch text ([`GhError::DiffTooLarge`]). Both are typed refusals, not
    /// truncations: a partial inventory that decoded cleanly is indistinguishable
    /// from the whole diff, and a coverage decision built on it would report a
    /// complete scope it never read. A server that declines to render the diff
    /// (`406`) is [`GhError::DiffUnavailable`], which is an *incomplete coverage*
    /// — never a clean review of a diff nobody read.
    ///
    /// The inventory is read as data: no file in it is compiled, imported, built,
    /// shelled or loaded.
    ///
    /// # Errors
    ///
    /// As [`Gh::snapshot`], plus [`GhError::DiffUnavailable`] when the server
    /// declines to render the diff, [`GhError::DiffFileCeiling`] and
    /// [`GhError::DiffTooLarge`] when it is past a declared bound, and
    /// [`GhError::Response`] or [`GhError::MalformedResponse`] when the answer is
    /// not a changed-file list.
    #[cfg(feature = "process")]
    pub async fn read_diff(&self, pull: &PullRequest) -> Result<PullDiff, GhError> {
        let path = format!(
            "repos/{}/pulls/{}/files",
            pull.repository().as_str(),
            pull.number()
        );
        let args = self.args_for(&["--method", "GET", &path, "--paginate"]);
        let outcome = self.call(&args).await?;
        if !outcome.succeeded() {
            // A `406` is the server declining to render a diff it considers too
            // large, which is the *unavailable diff* this read exists to report:
            // a typed coverage-incomplete answer, not a transport outage.
            if outcome.http_status() == Some(406) {
                let refusal = Err(GhError::DiffUnavailable {
                    path,
                    reason: outcome.stderr().trim().to_owned(),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read_diff: returning an error to the caller");
                return refusal;
            }
            outcome.require_success("reading the pull request's changed files")?;
        }
        let files = outcome.parse_json::<Vec<ChangedFile>>()?;
        if files.len() > MAX_DIFF_FILES_PER_PULL {
            let refusal = Err(GhError::DiffFileCeiling {
                path,
                files: files.len(),
                ceiling: MAX_DIFF_FILES_PER_PULL,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read_diff: returning an error to the caller");
            return refusal;
        }
        let patch_bytes = files.iter().fold(0usize, |total, file| {
            total.saturating_add(file.patch_bytes())
        });
        if patch_bytes > MAX_DIFF_BYTES {
            let refusal = Err(GhError::DiffTooLarge {
                path,
                bytes: patch_bytes,
                ceiling: MAX_DIFF_BYTES,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read_diff: returning an error to the caller");
            return refusal;
        }
        Ok(PullDiff {
            number: pull.number(),
            files,
            patch_bytes,
        })
    }

    /// Publish one review on `pull`, without the `process` feature.
    ///
    /// Nothing runs and nothing is reported as published. A review id of `0` here
    /// would be indistinguishable from a real answer in the places that consume
    /// it, and inventing one would be the module's exact defect: reporting an
    /// effect that was never attempted.
    ///
    /// # Errors
    ///
    /// [`GhError::NoRunner`], always. Rebuild with the `process` feature to bind
    /// this adapter to a supervised runner.
    #[cfg(not(feature = "process"))]
    pub async fn publish(
        &self,
        _pull: &PullRequest,
        _payload: &ReviewPayload,
    ) -> Result<u64, GhError> {
        Err(GhError::NoRunner)
    }

    /// Create one review on `pull` with `payload`.
    ///
    /// The payload is staged as a private file and named by a single `--input`
    /// argument, so a body containing quotes, newlines or a shell metacharacter
    /// is never re-split by anything. The reviewed commit is inside the
    /// payload, so a review cannot be published without naming the commit it is
    /// about.
    ///
    /// A successful return is the review's **id**, and it is still only
    /// transport evidence: it says GitHub accepted a create. Whether the
    /// review is about the intended commit and says the intended thing is
    /// [`Gh::read_reviews`]'s question, and this call's own success is not an
    /// answer to it.
    ///
    /// # Errors
    ///
    /// [`GhError::PayloadNotSent`] or [`GhError::Staging`] when the body could
    /// not be built, [`GhError::Transport`] when the client ran and failed, and
    /// [`GhError::Response`] or [`GhError::MalformedResponse`] when it answered
    /// with something that is not a created review.
    #[cfg(feature = "process")]
    pub async fn publish(
        &self,
        pull: &PullRequest,
        payload: &ReviewPayload,
    ) -> Result<u64, GhError> {
        let body = json::to_string(payload).map_err(|source| GhError::PayloadNotSent {
            reason: source.to_string(),
        })?;
        let path = format!(
            "repos/{}/pulls/{}/reviews",
            pull.repository().as_str(),
            pull.number()
        );
        // The staged file is this call's to remove. It is bound here, so it is
        // removed on every path out of this function, including the one where
        // the child never read it and the one where publishing succeeded.
        let staging = stage_input(body.as_bytes())?;
        let input_path = staging.path_str().to_owned();
        let args = self.args_for(&["--method", "POST", &path, "--input", &input_path]);
        // `gh api --input FILE` reads the request body from that file, so the
        // body stays one file rather than something a shell re-splits or a
        // per-argument length limit truncates.
        let outcome = self.run_spec(&args).await?;
        outcome.require_success("creating the review")?;
        let created = outcome.parse_json::<ReviewRecord>()?;
        if created.id() == 0 {
            let refusal = Err(GhError::Response {
                path,
                reason: String::from("the created review has no id"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "publish: returning an error to the caller");
            return refusal;
        }
        Ok(created.id())
    }
}

#[cfg(feature = "process")]
impl GhOutcome {
    /// Refuse a call that ran and did not succeed, naming the failure.
    ///
    /// # Errors
    ///
    /// [`GhError::Transport`] with the exit code, or [`GhError::Deadline`] when
    /// the supervisor stopped the child. Both say the call's own result is not
    /// known to have succeeded; neither says whether an effect landed, which is
    /// what a separate read-back is for.
    fn require_success(&self, what: &str) -> Result<(), GhError> {
        if self.deadline_fired {
            let refusal = Err(GhError::Deadline {
                what: what.to_owned(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "require_success: returning an error to the caller");
            return refusal;
        }
        match self.exit_code {
            Some(0) => Ok(()),
            Some(code) => {
                // `401` and `403` are a credential that cannot reach the
                // resource; `404` is a resource that is absent **or** not
                // visible to this credential, and the client cannot tell them
                // apart. For a read, both are "the resource's existence is not
                // established for this credential", which a caller must act on
                // as a permission outcome rather than as a transport outage, so
                // they are reported as [`GhError::Unauthorized`] rather than
                // flattened into [`GhError::Transport`]. A status the client did
                // not name stays a transport failure.
                if let Some(status @ (401 | 403 | 404)) = self.http_status() {
                    let refusal = Err(GhError::Unauthorized {
                        what: what.to_owned(),
                        status,
                        reason: self.stderr.clone(),
                    });
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "require_success: returning an error to the caller");
                    return refusal;
                }
                Err(GhError::Transport {
                    what: what.to_owned(),
                    exit_code: Some(code),
                    stderr: self.stderr.clone(),
                })
            }
            None => Err(GhError::Transport {
                what: what.to_owned(),
                exit_code: None,
                stderr: self.stderr.clone(),
            }),
        }
    }
}

/// Fold one supervised run into the observable outcome.
#[cfg(feature = "process")]
fn outcome_of(run: &ProcessRun) -> GhOutcome {
    let stdout = run.stdout();
    let stderr = run.stderr();
    GhOutcome {
        exit_code: run.exit_code(),
        stdout: String::from_utf8_lossy(stdout.bytes()).into_owned(),
        stderr: String::from_utf8_lossy(stderr.bytes()).into_owned(),
        stdout_truncated: stdout.truncated(),
        stderr_truncated: stderr.truncated(),
        stdout_total_bytes: stdout.total_bytes(),
        stderr_total_bytes: stderr.total_bytes(),
        deadline_fired: run.deadline_fired(),
        cleanup_confirmed: matches!(
            run.cleanup(),
            crate::rt::supervise::CleanupReceipt::CleanupConfirmed
        ),
    }
}

/// A name suffix no two staged payloads share, or the empty string when this
/// build has no distinguishable source.
///
/// `lgwks_std::random` where the build has it, because a process id is reused
/// by the OS and two processes staging a payload under the same name would
/// race for one file. Without the `ephemeral` feature there is no entropy
/// source this crate may assume, so the name is a per-process monotone counter
/// and the `create_new` below is what turns a cross-process collision into a
/// refusal rather than an overwrite. That is a stated limit, not an identity.
#[cfg(feature = "process")]
fn unique_tag() -> Option<String> {
    #[cfg(feature = "ephemeral")]
    {
        // An entropy failure is no tag at all, never a fixed fallback: a
        // constant name is the collision the tag exists to prevent.
        let bytes = lgwks_std::random::bytes::<8>().ok()?;
        Some(bytes.iter().fold(String::new(), |mut text, byte| {
            use std::fmt::Write as _;
            // A `write!` into a `String` is infallible; the result is bound
            // rather than discarded so nothing in this crate can ignore a
            // formatting failure by accident.
            let _written = write!(text, "{byte:02x}");
            text
        }))
    }
    // Without the `ephemeral` feature this crate may not assume an entropy
    // source (INV-DEP-6), so there is no tag to give. Rather than fall back to
    // something the OS reuses — which is how two concurrent publications end
    // up refusing each other's staged file — the publish path says it cannot
    // run, and the review task reports that rather than publishing under a name
    // it cannot guarantee. A `Refused` is the honest answer here.
    #[cfg(not(feature = "ephemeral"))]
    {
        None
    }
}

/// A payload staged on disk for one `gh --input` call.
///
/// The alternative is a second process path with a piped stdin, and the crate
/// has exactly one sanctioned runner. A staged file is a resource with a known
/// lifetime and an owner, which is the shape the estate's rules already
/// require; a private spawner would be neither.
///
/// The path is held as UTF-8 text because it becomes a command-line argument;
/// [`stage_input`] refuses a temporary directory that is not UTF-8 rather than
/// lossily converting it into a name for a different file.
#[cfg(feature = "process")]
struct StagedInput(String);

#[cfg(feature = "process")]
impl StagedInput {
    /// The path as an argument, which is a `String` the `ProcessSpec` takes.
    ///
    /// Returned by reference so the argument vector is built once and the
    /// staged file stays owned by exactly one value — the one whose `Drop`
    /// removes it.
    fn path_str(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "process")]
impl Drop for StagedInput {
    /// Remove the staged payload, best effort.
    ///
    /// A removal that fails leaves the JSON of a not-yet-published review on
    /// disk. That is a privacy property, not a correctness one, so it is
    /// reported as a `log`-level warning rather than turning a successful
    /// publication into a failure.
    fn drop(&mut self) {
        if let Err(cause) = std::fs::remove_file(&self.0)
            && cause.kind() != std::io::ErrorKind::NotFound
        {
            lgwks_std::trace::warn!(
                path = %self.0,
                error = %cause,
                "the staged review payload could not be removed"
            );
        }
    }
}

/// Write `input` to a private staging file and return its owner.
///
/// The file is mode 0600 from creation, and it lives in the system temporary
/// directory, so a review body that has not been published is not world
/// readable on a shared machine.
#[cfg(feature = "process")]
fn stage_input(input: &[u8]) -> Result<StagedInput, GhError> {
    use std::io::Write as _;

    let mut path = std::env::temp_dir();
    // `create_new` below is what makes a collision a refusal rather than an
    // overwrite, so the name is the only thing that has to differ.
    let Some(tag) = unique_tag() else {
        let refusal = Err(GhError::Staging {
            path: path.display().to_string(),
            source: String::from(
                "no entropy source to name the staged payload uniquely; a build without the \
             `ephemeral` feature refuses to publish rather than reuse a name",
            ),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "stage_input: returning an error to the caller");
        return refusal;
    };
    path.push(format!("lgwks-gh-payload-{tag}.json"));
    let Some(text) = path.to_str().map(str::to_owned) else {
        let refusal = Err(GhError::Staging {
            path: path.display().to_string(),
            source: String::from(
                "the temporary directory is not UTF-8, so it cannot be an argument",
            ),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "stage_input: returning an error to the caller");
        return refusal;
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|source| GhError::Staging {
        path: path.display().to_string(),
        source: source.to_string(),
    })?;
    file.write_all(input).map_err(|source| GhError::Staging {
        path: path.display().to_string(),
        source: source.to_string(),
    })?;
    file.sync_all().map_err(|source| GhError::Staging {
        path: path.display().to_string(),
        source: source.to_string(),
    })?;
    Ok(StagedInput(text))
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// Why one GitHub adapter call could not produce what it was asked for.
///
/// The variants draw the distinctions a caller's next move turns on: a client
/// that never ran, a client that ran and failed, an answer that was not the
/// shape expected, and an answer that was truncated. None of them is a claim
/// about whether an effect landed — that is [`Gh::read_reviews`]'s question.
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

impl GhError {
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
            | Self::Transport { .. }
            | Self::Deadline { .. }
            | Self::Response { .. }
            | Self::TruncatedResponse { .. }
            | Self::ReviewCeiling { .. }
            | Self::Unauthorized { .. }
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
        let certainty = match source {
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
            | GhError::NoRunner
            | GhError::PayloadNotSent { .. }
            | GhError::Staging { .. } => crate::error::DispatchCertainty::Refused,
            // The client ran: whatever it was asked to do may have happened
            // and its answer been lost or misread.
            _ => crate::error::DispatchCertainty::NotDelivered,
        };
        let cause = match certainty {
            crate::error::DispatchCertainty::Unsettled => {
                crate::error::BotError::EffectIndeterminate {
                    domain: String::from("gh"),
                    cause: source.to_string(),
                }
            }
            _ => crate::error::BotError::DomainError {
                domain: String::from("gh"),
                certainty,
                cause: source.to_string(),
            },
        };
        crate::script::FlowError::Bot {
            at: std::sync::Arc::from(""),
            source: Box::new(cause),
        }
    }
}

// ── The four verbs over the `gh` binding ────────────────────────────────────

/// The GitHub binding as a query surface: a read-only `gh api` call.
///
/// A `Query` here is a real, bounded, supervised `gh api` invocation whose
/// stdout the caller parses. It exists so the same adapter serves an ordinary
/// caller through the verbs rather than through a private helper, which is what
/// makes a verb-level capability denial observable on the real path.
#[derive(Debug)]
pub struct GhQuery {
    /// The binding every call runs through.
    gh: Gh,
    /// The argument vector after `api`.
    rest: Vec<String>,
    /// Forced by the constructor: the same two capabilities [`Gh`] requires.
    caps: Vec<Cap>,
}

impl GhQuery {
    /// Query `gh` with these arguments.
    #[must_use]
    pub fn new(gh: Gh, rest: impl IntoIterator<Item = String>) -> Self {
        Self {
            caps: gh.required_caps().to_vec(),
            gh,
            rest: rest.into_iter().collect(),
        }
    }
}

impl verb::Query for GhQuery {
    type Input = ();
    type Output = GhOutcome;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn query(&self, call: (Auth, &())) -> Result<GhOutcome, BotError> {
        // Checked before anything is started, so a denial is a refusal and not
        // a child process that ran and whose output nobody wanted.
        call.0.check(self.required_caps())?;
        self.run().await
    }

    fn domain_id(&self) -> &str {
        "gh::api"
    }
}

impl GhQuery {
    /// The call itself, shared by every build of this surface.
    #[cfg(not(feature = "process"))]
    async fn run(&self) -> Result<GhOutcome, BotError> {
        let rest: Vec<&str> = self.rest.iter().map(String::as_str).collect();
        let args = self.gh.args_for(&rest);
        match self.gh.call(&args).await {
            // Nothing ran, so a retry cannot duplicate anything.
            Err(GhError::NoRunner) | Ok(GhOutcome { .. }) => Err(BotError::DomainError {
                domain: String::from("gh::api"),
                certainty: DispatchCertainty::Refused,
                cause: GhError::NoRunner.to_string(),
            }),
            Err(source) => Err(BotError::DomainError {
                domain: String::from("gh::api"),
                certainty: DispatchCertainty::NotDelivered,
                cause: source.to_string(),
            }),
        }
    }

    /// The call itself, shared by every build of this surface.
    #[cfg(feature = "process")]
    async fn run(&self) -> Result<GhOutcome, BotError> {
        let rest: Vec<&str> = self.rest.iter().map(String::as_str).collect();
        let args = self.gh.args_for(&rest);
        match self.gh.call(&args).await {
            Ok(outcome) => Ok(outcome),
            Err(GhError::Process(crate::rt::process::ProcessRunError::Refused))
            | Err(GhError::Process(crate::rt::process::ProcessRunError::NotStarted { .. })) => {
                // Nothing ran, so a retry cannot duplicate.
                Err(BotError::DomainError {
                    domain: String::from("gh::api"),
                    certainty: DispatchCertainty::Refused,
                    cause: format!("the GitHub client did not start: {}", self.gh.program),
                })
            }
            // The child started: whatever it was about to do may have happened,
            // so this is indeterminate rather than a clean failure.
            Err(GhError::Process(crate::rt::process::ProcessRunError::AfterStart { source })) => {
                Err(BotError::EffectIndeterminate {
                    domain: String::from("gh::api"),
                    cause: format!("the GitHub client started but did not settle: {source}"),
                })
            }
            Err(source) => Err(BotError::DomainError {
                domain: String::from("gh::api"),
                certainty: DispatchCertainty::NotDelivered,
                cause: source.to_string(),
            }),
        }
    }
}

/// The GitHub binding as an observe surface: repeated snapshots of a pull
/// request's head.
///
/// Each poll is one supervised `gh` call, so an observer that fires repeatedly
/// is a bounded sequence of real children rather than a cached value. The state
/// it reports is the head sha, which is the fact a freshness check turns on.
#[derive(Debug)]
pub struct PrSnapshotSource {
    /// The binding every poll runs through.
    gh: Gh,
    /// The pull request observed.
    pull: PullRequest,
    /// Forced by the constructor: the same two capabilities [`Gh`] requires.
    caps: Vec<Cap>,
}

impl PrSnapshotSource {
    /// Observe `pull` through `gh`.
    #[must_use]
    pub fn new(gh: Gh, pull: PullRequest) -> Self {
        Self {
            caps: gh.required_caps().to_vec(),
            gh,
            pull,
        }
    }
}

impl verb::Observe for PrSnapshotSource {
    type Output = PrSnapshot;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<PrSnapshot, BotError> {
        call.0.check(self.required_caps())?;
        match self.gh.snapshot(&self.pull).await {
            Ok(snapshot) => Ok(snapshot),
            Err(source) => Err(BotError::DomainError {
                domain: self.domain_id().into(),
                certainty: DispatchCertainty::NotDelivered,
                cause: source.to_string(),
            }),
        }
    }

    fn domain_id(&self) -> &str {
        "gh::pr_snapshot"
    }
}
