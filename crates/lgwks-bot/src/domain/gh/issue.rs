//! `gh::issue`: open one GitHub issue per caller key.
//!
//! GitHub's create-issue request has no idempotency key, so the key travels in
//! the issue itself, as an HTML comment trailer (`<!-- lgwks-issue:KEY -->`)
//! that renders as nothing. Keeping one issue per key is not this module's
//! work: [`crate::keyed`] does that for every receiver without an idempotency
//! key, and this module is its GitHub adapter. It supplies what is GitHub's:
//! how a key's issue is found, how one is created, which failed creates
//! landed nothing, and GitHub's own bound on a request in flight.
//!
//! # Finding a key: the listing lags; the numbers do not
//!
//! A find reads the repository's newest [`MAX_ISSUES_READ_BACK`] issues. The
//! listing lags a create by seconds (the first live open, #657, was missing
//! from the read made right after GitHub answered it, and an immediate rerun
//! then opened #659 beside #658), while an issue's own address answers at once
//! and issue numbers are one sequence per repository. So a find that does not
//! see the key in the listing reads by number what the listing may not have
//! caught up on:
//!
//! - the numbers just below the listing's newest that it did not list, since
//!   an older issue can trail a newer one into the listing;
//! - every number past the listing's newest, until two in a row do not exist.
//!   One missing number is not the end: Discussions take numbers from the same
//!   sequence and answer `404` at an issue's address.
//!
//! # Bounds, stated
//!
//! An issue older than the newest [`MAX_ISSUES_READ_BACK`] is not found. One
//! find reads at most 16 numbers; a repository whose listing trails further
//! than that refuses the open rather than creating over a window it could not
//! read. Two Discussions in a row past the listing's newest end a scan early. GitHub terminates a request after 10 seconds, so a create still
//! in flight lands within that (plus a second for a request still in transit);
//! [`crate::keyed`] waits that out before a new key's create.

use std::time::Duration;

use lgwks_std::json::{self, Deserialize, Serialize};

#[cfg(any(feature = "ecs", feature = "script", feature = "inspect"))]
use super::Repository;
use super::{Gh, GhError, stage_input};
#[cfg(any(feature = "ecs", feature = "script", feature = "inspect"))]
use crate::Action;
use crate::cap::{Auth, Cap};
use crate::clock::Clock;
use crate::error::BotError;
pub use crate::keyed::Settlement;
use crate::keyed::{self, CreateFailure, KeyedReceiver};
use crate::verb;

/// The capability that lets a run open an issue, beyond running `gh` at all.
///
/// Its own name rather than `bot.net`: reaching GitHub to read is not the
/// authority to write to it, and a host grants the write by naming it.
pub const ISSUE_CREATE: &str = "github.issue.create";

/// How many of a repository's newest issues one find looks through.
///
/// GitHub's page ceiling, read as one page: the read is bounded by this
/// number, never by the repository's history.
pub const MAX_ISSUES_READ_BACK: usize = 100;

/// How many issue numbers one find reads past what the listing showed.
///
/// GitHub's listing trails its creates by a few issues at most; a repository
/// opening more than this many inside one listing lag is refused rather than
/// scanned partially, because a create over a window nobody read is the
/// duplicate this action exists to prevent.
const MAX_UNLISTED_READS: u64 = 16;

/// How many numbers below the listing's newest a find checks for an issue the
/// listing skipped.
const GAP_WINDOW: u64 = 8;

/// How many missing numbers in a row past the listing end a find.
const END_OF_NUMBERS: u8 = 2;

/// GitHub's bound on processing one request, plus a second for one still in
/// transit when its sender died.
///
/// "If GitHub takes more than 10 seconds to process an API request, GitHub
/// will terminate the request" (GitHub, Troubleshooting the REST API).
const IN_FLIGHT_BOUND: Duration = Duration::from_secs(11);

/// The domain identifier a registry declares this action under.
const DOMAIN: &str = "gh::issue";

/// GitHub's own ceiling on an issue title, in characters.
const MAX_TITLE_CHARS: usize = 256;

/// GitHub's own ceiling on an issue body, in characters, trailer included.
const MAX_BODY_CHARS: usize = 65_536;

/// The most bytes a caller key may hold.
const MAX_KEY_BYTES: usize = 64;

/// One issue to open, bound to the caller key that makes opening it once.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct IssueDraft {
    /// The caller's key: one key, one issue.
    key: String,
    /// The title, as GitHub will hold it.
    title: String,
    /// The body exactly as it is sent, key trailer included.
    body: String,
}

impl IssueDraft {
    /// Bind `title` and `body` to `key`.
    ///
    /// # Errors
    ///
    /// [`GhError::IssueDraft`] when the key is empty, longer than 64 bytes or
    /// holds anything but ASCII letters, digits, `-`, `_`, `.` and `:`; when the
    /// title is blank, holds a control character or is longer than GitHub's 256
    /// characters; or when the body with its trailer is longer than GitHub's
    /// 65,536 characters. Each is refused here, before any call, because GitHub
    /// would refuse it after one.
    pub fn new(
        key: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Self, GhError> {
        let key = key.into();
        let title = title.into();
        let refuse = |reason: &'static str| {
            let refusal = Err(GhError::IssueDraft { reason });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            refusal
        };
        if key.is_empty() || key.len() > MAX_KEY_BYTES {
            return refuse("a key holds 1 to 64 bytes");
        }
        if !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        {
            return refuse(
                "a key travels inside an HTML comment, so it holds ASCII letters, digits, \
                 '-', '_', '.' and ':' only",
            );
        }
        if title.trim().is_empty() || title.contains(char::is_control) {
            return refuse("a title is one line with something on it");
        }
        if title.chars().count() > MAX_TITLE_CHARS {
            return refuse("GitHub holds at most 256 characters of title");
        }
        let mut body = body.into();
        body.push_str("\n\n");
        body.push_str(&trailer(&key));
        if body.chars().count() > MAX_BODY_CHARS {
            return refuse("GitHub holds at most 65,536 characters of body, key trailer included");
        }
        Ok(Self { key, title, body })
    }

    /// What a rerun finds this issue by: the token written into the body's
    /// hidden trailer, unique per issue on one repository.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// One line of at most 256 characters, compared on read-back so a key
    /// reused for another issue is refused rather than answered.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The body exactly as it is sent, key trailer included.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// The hidden trailer a find reads `key` by.
fn trailer(key: &str) -> String {
    format!("<!-- lgwks-issue:{key} -->")
}

/// The issue an open settled on.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenedIssue {
    /// The issue number.
    number: u64,
    /// The issue's page.
    url: String,
    /// How the open came to it.
    settlement: Settlement,
}

impl OpenedIssue {
    /// GitHub's per-repository sequence for the issue, never zero; `#N` in a
    /// comment links it.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }

    /// The issue's page on GitHub.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// How the open came to it.
    #[must_use]
    pub const fn settlement(&self) -> Settlement {
        self.settlement
    }
}

/// What the create request sends.
#[derive(Serialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct IssueWire<'draft> {
    /// The title.
    title: &'draft str,
    /// The body, trailer included.
    body: &'draft str,
}

/// The fields of an issue a find compares: exactly what [`keyed_projection`]
/// keeps, so no body crosses the capture ceiling.
#[derive(Debug, Default, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct IssueRecord {
    /// The issue number; `0` is no issue.
    #[serde(default)]
    number: u64,
    /// The issue's page.
    #[serde(default)]
    html_url: String,
    /// The title.
    #[serde(default)]
    title: String,
    /// Whether it is an issue, not a pull request, whose body carries the
    /// scanned key's trailer.
    #[serde(default)]
    keyed: bool,
}

/// What one listing read saw.
#[derive(Debug, Default, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct Listing {
    /// The highest number the listing reached, issue or pull request; `0`
    /// when the repository lists nothing.
    #[serde(default)]
    newest: u64,
    /// Every number the listing showed, issue or pull request.
    #[serde(default)]
    numbers: Vec<u64>,
    /// The issues carrying the key, newest first.
    #[serde(default)]
    keyed: Vec<IssueRecord>,
}

/// The `gh --jq` projection every issue is cut to before it is printed.
///
/// GitHub answers an issue with its whole body, and a hundred bodies run to
/// megabytes, past any capture a supervised child is given (a live read of
/// this repository's newest 100 issues was 567,275 bytes against a 65,536-byte
/// ceiling). `gh` evaluates the filter in its own process, so what is captured
/// is four fields per issue, whatever the bodies hold. The key's alphabet
/// (`[A-Za-z0-9._:-]`, enforced by [`IssueDraft::new`]) holds no quote or
/// backslash, so it is a literal inside the filter's string.
fn keyed_projection(key: &str) -> String {
    format!(
        "{{number, html_url, title, keyed: (.pull_request == null and ((.body // \"\") | contains(\"{}\")))}}",
        trailer(key)
    )
}

/// The filter that cuts a page of issues to the numbers on it and the issues
/// carrying `key`, newest first, projected.
fn listing_filter(key: &str) -> String {
    format!(
        "{{newest: ([.[].number] | max // 0), numbers: [.[].number], keyed: [.[] | {} | select(.keyed)]}}",
        keyed_projection(key)
    )
}

impl IssueRecord {
    /// This record as the issue an open settled on.
    fn settled(self, settlement: Settlement) -> OpenedIssue {
        OpenedIssue {
            number: self.number,
            url: self.html_url,
            settlement,
        }
    }

    /// This record as `draft`'s issue, or the refusal of a key that names an
    /// issue about something else.
    ///
    /// One key names one issue, and handing back an issue with another title
    /// would report an open that never happened.
    fn for_draft(self, draft: &IssueDraft) -> Result<Self, GhError> {
        if self.title == draft.title() {
            return Ok(self);
        }
        let refusal = Err(GhError::Response {
            path: self.html_url,
            reason: format!(
                "the key {} already names issue #{} with another title; a key names one issue",
                draft.key(),
                self.number
            ),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "for_draft: returning an error to the caller");
        refusal
    }
}

impl Gh {
    /// One listing read: the numbers the repository lists among its newest
    /// [`MAX_ISSUES_READ_BACK`] issues, and the ones carrying `key`.
    async fn listing(&self, key: &str) -> Result<Listing, GhError> {
        let path = format!(
            "repos/{}/issues?state=all&sort=created&direction=desc&per_page={MAX_ISSUES_READ_BACK}",
            self.repository.as_str()
        );
        let filter = listing_filter(key);
        let args = self.args_for(&["--method", "GET", &path, "--jq", &filter]);
        let outcome = self.call(&args).await?;
        outcome.require_success("reading the repository's newest issues")?;
        let listing = outcome.parse_json::<Listing>()?;
        if listing.keyed.len() > MAX_ISSUES_READ_BACK
            || listing.numbers.len() > MAX_ISSUES_READ_BACK
        {
            let refusal = Err(GhError::Response {
                path,
                reason: format!(
                    "{} issues answered a read of at most {MAX_ISSUES_READ_BACK}",
                    listing.numbers.len().max(listing.keyed.len())
                ),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "listing: returning an error to the caller");
            return refusal;
        }
        Ok(listing)
    }

    /// Issue `number` as `key`'s find sees it, or `None` when GitHub holds no
    /// issue at that number.
    ///
    /// A `404` right after a listing this credential read is a number that is
    /// not an issue — not used yet, or a Discussion's — not a permission loss.
    /// A `410` is a deleted issue: the number is spent and carries no key, so
    /// it reads as an issue that is not this key's.
    async fn numbered(&self, number: u64, key: &str) -> Result<Option<IssueRecord>, GhError> {
        let path = format!("repos/{}/issues/{number}", self.repository.as_str());
        let filter = keyed_projection(key);
        let args = self.args_for(&["--method", "GET", &path, "--jq", &filter]);
        let outcome = self.call(&args).await?;
        if !outcome.succeeded() && !outcome.deadline_fired() {
            match outcome.http_status() {
                Some(404) => return Ok(None),
                Some(410) => {
                    return Ok(Some(IssueRecord {
                        number,
                        ..IssueRecord::default()
                    }));
                }
                _ => {}
            }
        }
        outcome.require_success("reading an issue past the listing")?;
        outcome.parse_json::<IssueRecord>().map(Some)
    }

    /// The first issue opened under `draft`'s key, listed or not yet listed.
    ///
    /// The listing's oldest match, not its newest: when a key ever names two
    /// issues (the live duplicate #659 beside #658), every later run answers
    /// the one the key was opened as. When the listing holds no match, the
    /// numbers it may not have caught up on are read in turn (see the module
    /// documentation).
    ///
    /// A read that fails is returned rather than read past: a create issued
    /// over a window nobody could read is the duplicate this find exists to
    /// prevent.
    async fn issue_keyed(&self, draft: &IssueDraft) -> Result<Option<IssueRecord>, GhError> {
        let listing = self.listing(draft.key()).await?;
        if let Some(listed) = listing.keyed.into_iter().last() {
            return listed.for_draft(draft).map(Some);
        }
        let mut reads = 0_u64;
        let skipped = (listing.newest.saturating_sub(GAP_WINDOW).max(1)..listing.newest)
            .filter(|number| !listing.numbers.contains(number));
        for number in skipped {
            reads = self.unlisted_read(reads, listing.newest)?;
            if let Some(unlisted) = self.numbered(number, draft.key()).await?
                && unlisted.keyed
            {
                return unlisted.for_draft(draft).map(Some);
            }
        }
        let mut number = listing.newest;
        let mut missing = 0_u8;
        while missing < END_OF_NUMBERS {
            reads = self.unlisted_read(reads, listing.newest)?;
            number = number.saturating_add(1);
            match self.numbered(number, draft.key()).await? {
                None => missing = missing.saturating_add(1),
                Some(unlisted) if unlisted.keyed => return unlisted.for_draft(draft).map(Some),
                Some(_) => missing = 0,
            }
        }
        Ok(None)
    }

    /// Count one read past the listing, or refuse the find once it would pass
    /// [`MAX_UNLISTED_READS`].
    fn unlisted_read(&self, reads: u64, newest: u64) -> Result<u64, GhError> {
        if reads < MAX_UNLISTED_READS {
            return Ok(reads.saturating_add(1));
        }
        let refusal = Err(GhError::Response {
            path: format!("repos/{}/issues", self.repository.as_str()),
            reason: format!(
                "more than {MAX_UNLISTED_READS} issue numbers around #{newest}, the newest the \
                 listing reached, are not listed, so the find could not read the window a create \
                 would land in"
            ),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "unlisted_read: returning an error to the caller");
        refusal
    }

    /// Create `draft` once, through a staged `--input` payload, and confirm it
    /// from GitHub's answer.
    ///
    /// The `201` answer is the issue as GitHub stored it, so its number, its
    /// title and the trailer in its body are read from that answer; an answer
    /// that does not show all three is not a confirmation, and the open reads
    /// back as it would after a lost answer.
    async fn create_issue(&self, draft: &IssueDraft) -> Result<IssueRecord, GhError> {
        let wire = IssueWire {
            title: draft.title(),
            body: draft.body(),
        };
        let body = json::to_string(&wire).map_err(|source| GhError::PayloadNotSent {
            reason: source.to_string(),
        })?;
        let path = format!("repos/{}/issues", self.repository.as_str());
        // Bound here so the staged file is removed on every path out.
        let staging = stage_input(body.as_bytes())?;
        let projection = keyed_projection(draft.key());
        let args = self.args_for(&[
            "--method",
            "POST",
            &path,
            "--input",
            staging.path_str(),
            "--jq",
            &projection,
        ]);
        let outcome = self.run_spec(&args).await?;
        if !outcome.succeeded()
            && !outcome.deadline_fired()
            && let Some(status @ (400 | 410 | 422)) = outcome.http_status()
        {
            let refusal = Err(GhError::Rejected {
                what: String::from("opening the issue"),
                status,
                reason: outcome.stderr().trim().to_owned(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "create_issue: returning an error to the caller");
            return refusal;
        }
        outcome.require_success("opening the issue")?;
        let created = outcome.parse_json::<IssueRecord>()?;
        if created.number == 0 || !created.keyed || created.title != draft.title() {
            let refusal = Err(GhError::Response {
                path,
                reason: format!(
                    "the create's answer (#{}) does not show key {} and its title",
                    created.number,
                    draft.key()
                ),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "create_issue: returning an error to the caller");
            return refusal;
        }
        Ok(created)
    }
}

/// GitHub as a [`KeyedReceiver`]: issues found by their key's trailer.
struct Issues<'gh> {
    /// The binding every call runs through.
    gh: &'gh Gh,
}

impl KeyedReceiver for Issues<'_> {
    type Draft = IssueDraft;
    type Held = IssueRecord;
    type Error = GhError;

    fn find(
        &self,
        draft: &IssueDraft,
    ) -> impl Future<Output = Result<Option<IssueRecord>, GhError>> + Send {
        self.gh.issue_keyed(draft)
    }

    fn create(
        &self,
        draft: &IssueDraft,
    ) -> impl Future<Output = Result<IssueRecord, GhError>> + Send {
        self.gh.create_issue(draft)
    }

    /// A create that never started, whose credential GitHub refused, or that
    /// GitHub rejected as a request landed nothing; anything else may have.
    fn create_failure(&self, failure: &GhError) -> CreateFailure {
        let nothing = matches!(
            *failure,
            GhError::IssueDraft { .. }
                | GhError::PayloadNotSent { .. }
                | GhError::Staging { .. }
                | GhError::NoRunner
                | GhError::Unauthorized { .. }
                | GhError::Rejected { .. }
                | GhError::Process(
                    crate::rt::process::ProcessRunError::Refused
                        | crate::rt::process::ProcessRunError::NotStarted { .. }
                )
        );
        if nothing {
            CreateFailure::NothingLanded
        } else {
            CreateFailure::MayHaveLanded
        }
    }

    fn in_flight_bound(&self) -> Duration {
        IN_FLIGHT_BOUND
    }
}

/// The `gh::issue` action: open a [`IssueDraft`] on the repository its target
/// names, once per key.
///
/// It requires `bot.sys` and `bot.net`, which running `gh` is, and
/// [`ISSUE_CREATE`], which writing to GitHub is; a run short of any of them is
/// blocked before `gh` starts.
#[derive(Debug)]
pub struct OpenIssue {
    /// The binding every call runs through.
    gh: Gh,
    /// What the binding requires, plus [`ISSUE_CREATE`].
    caps: Vec<Cap>,
}

impl OpenIssue {
    /// Open issues through `gh`.
    #[must_use]
    pub fn new(gh: Gh) -> Self {
        let mut caps = gh.required_caps().to_vec();
        caps.push(Cap::new(ISSUE_CREATE));
        Self { gh, caps }
    }

    /// The registry constructor: `target` is the `owner/repo` to open issues on,
    /// reached through `gh` on `PATH`.
    ///
    /// # Errors
    ///
    /// [`BotError::IncompleteSpec`] naming the target when it is not
    /// `owner/repo`.
    #[cfg(any(feature = "ecs", feature = "script", feature = "inspect"))]
    pub fn from_target(target: &str) -> Result<Action, BotError> {
        let repository = Repository::new(target).map_err(|refused| BotError::IncompleteSpec {
            field: "target",
            cause: refused.to_string(),
        })?;
        Ok(Action::new(Self::new(Gh::new(repository))))
    }
}

impl verb::Execute for OpenIssue {
    type Input = IssueDraft;
    type Output = OpenedIssue;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn execute_action(&self, call: (Auth, &IssueDraft)) -> Result<OpenedIssue, BotError> {
        // The call that carried no flow scope measures on the process's wall
        // clock, which is the live schedule GitHub's bound needs.
        self.execute_action_on(&Clock::wall(), call).await
    }

    async fn execute_action_on(
        &self,
        clock: &Clock,
        call: (Auth, &IssueDraft),
    ) -> Result<OpenedIssue, BotError> {
        // Checked before `gh` starts, so a denial created and read nothing.
        call.0.check(self.required_caps())?;
        let issues = Issues { gh: &self.gh };
        let (record, settlement) = keyed::open_once(&issues, call.1, clock)
            .await
            .map_err(|failure| failure.into_bot_error(DOMAIN, |gh| gh.into_bot_error(DOMAIN)))?;
        Ok(record.settled(settlement))
    }

    fn domain_id(&self) -> &str {
        DOMAIN
    }
}
