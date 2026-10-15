//! The read-back record: reviews, their authors and comments, a pull's diff, and its snapshot.

use lgwks_std::json::{Deserialize, Serialize};

#[cfg(feature = "process")]
use super::Repository;
use super::ReviewPayload;

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
        let Some(count) = self.comment_count else {
            return 0;
        };
        // Saturation rather than a second default: a count wider than this
        // host's `usize` is more comments than this process could have
        // intended, because the intended side is a slice length, so it cannot
        // match any payload either way. Reporting zero would additionally claim
        // that no comment landed where the receiver said more than could be
        // counted.
        match usize::try_from(count) {
            Ok(applied) => applied,
            Err(_wider_than_this_host_counts) => usize::MAX,
        }
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
        // No patch carries no patch bytes: zero is the documented count, not a
        // default standing in for an unknown one.
        match self.patch.as_ref() {
            Some(patch) => patch.len(),
            None => 0,
        }
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
    pub(super) number: u64,
    /// The changed files, in the order the receiver reported them.
    pub(super) files: Vec<ChangedFile>,
    /// Total bytes of patch text, summed over every changed file.
    pub(super) patch_bytes: usize,
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
/// [`Gh::snapshot`](super::Gh::snapshot) refuses rather than reviewing nothing.
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
pub(super) struct MovedWire {
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
    pub(super) fn canonical(&self) -> Option<String> {
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
