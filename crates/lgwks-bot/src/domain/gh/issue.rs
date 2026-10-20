//! `gh::issue`: open one GitHub issue per caller key, settled by reading
//! GitHub back rather than by trusting a create's answer.
//!
//! GitHub's create-issue request has no idempotency key, so the key travels in
//! the issue itself, as an HTML comment trailer (`<!-- lgwks-issue:KEY -->`)
//! that renders as nothing. Every open reads the repository's newest
//! [`MAX_ISSUES_READ_BACK`] issues first, so a second run with the same key —
//! a rerun, a resume after a kill, a retry after a lost answer — finds the
//! first run's issue instead of opening another.
//!
//! # The listing lags; the numbers do not
//!
//! GitHub's listing lags a create by seconds (the first live open, #657, was
//! missing from the read made right after GitHub answered it, and an immediate
//! rerun then opened #659 beside #658). An issue's own address answers at
//! once, and issue numbers are one sequence per repository, shared with pull
//! requests. So a scan that does not find the key in the listing reads every
//! number past the newest one the listing reached, until GitHub answers that
//! a number does not exist yet. What the listing has not caught up on is
//! exactly that run of numbers, so the scan sees it whole in one listing read
//! and, in the common case, one 404 — never a wait for the listing to catch
//! up. That is what makes a new key cost one listing read, one number read
//! and the create, rather than a settle schedule of reads and sleeps.
//!
//! A create is confirmed from GitHub's own `201` answer, which carries the
//! stored issue: its number, its title and the key trailer in its body.
//!
//! # The one path that still waits
//!
//! A create whose answer was lost — a reset connection, a `kill -9` between
//! the POST and its reply — may still be landing when this run reads back, so
//! that read-back alone repeats the scan over a bounded schedule on the flow's
//! [`Clock`]. One that still cannot find the key is
//! [`BotError::EffectIndeterminate`], never a second create. A create GitHub
//! refused outright (`400`, `410`, `422`) landed nothing and is reported as
//! that refusal at once.
//!
//! # Bounds, stated
//!
//! An issue older than the newest [`MAX_ISSUES_READ_BACK`] in its repository
//! is not found, and a key reused after that many newer issues opens a new
//! one. A scan reads at most 16 numbers past the listing;
//! a repository whose listing is further behind than that refuses the open
//! rather than creating over a window it could not read. Two runs that open
//! the same key at the same instant, on hosts that share nothing, can both
//! find nothing and both create: GitHub offers no conditional create, and the
//! key makes every later run answer the first of them.

use std::time::Duration;

use lgwks_std::json::{self, Deserialize, Serialize};

use super::{Gh, GhError, Repository, stage_input};
use crate::Action;
use crate::cap::{Auth, Cap};
use crate::clock::Clock;
use crate::error::BotError;
use crate::verb;

/// The capability that lets a run open an issue, beyond running `gh` at all.
///
/// Its own name rather than `bot.net`: reaching GitHub to read is not the
/// authority to write to it, and a host grants the write by naming it.
pub const ISSUE_CREATE: &str = "github.issue.create";

/// How many of a repository's newest issues one read-back looks through.
///
/// GitHub's page ceiling, read as one page: the read is bounded by this
/// number, never by the repository's history.
pub const MAX_ISSUES_READ_BACK: usize = 100;

/// How many numbers past the listing's newest one scan reads by number.
///
/// GitHub's listing trails its creates by a few issues at most; a repository
/// opening more than this many issues inside one listing lag is refused
/// rather than scanned partially, because a create over a window nobody read
/// is the duplicate this action exists to prevent.
const MAX_UNLISTED_READS: u64 = 16;

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

/// The hidden trailer a read-back finds `key` by.
fn trailer(key: &str) -> String {
    format!("<!-- lgwks-issue:{key} -->")
}

/// How an open came to its issue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Settlement {
    /// This run created it, and GitHub's answer to the create shows it.
    Created,
    /// An earlier run with this key had opened it; nothing was created.
    Found,
    /// This run's create lost its answer, and the read-back found the issue.
    Reconciled,
}

impl Settlement {
    /// The lowercase word a report prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Found => "found",
            Self::Reconciled => "reconciled",
        }
    }
}

/// How many scans the read-back after a lost create answer makes.
const SETTLE_SCANS: u8 = 8;

/// The wait before the read-back's second scan; each later wait doubles.
const FIRST_SETTLE_DELAY: Duration = Duration::from_millis(250);

/// The longest single wait between the read-back's scans.
const MAX_SETTLE_DELAY: Duration = Duration::from_secs(4);

/// The bounded schedule the read-back after a lost create answer follows.
///
/// The only wait in this module: a create whose answer was lost may still be
/// landing on GitHub's side when the read-back starts, and a single scan that
/// misses it would report an indeterminate create. [`SETTLE_SCANS`] scans: the
/// first immediately, the wait before each later one starting at
/// [`FIRST_SETTLE_DELAY`] and doubling up to [`MAX_SETTLE_DELAY`], about 16 s
/// on the flow's clock in all.
struct Settle {
    /// The wait to make before the next scan.
    next: Duration,
    /// How many scans this settle has made.
    made: u8,
}

impl Settle {
    /// A settle with every scan still to make.
    const fn new() -> Self {
        Self {
            next: FIRST_SETTLE_DELAY,
            made: 0,
        }
    }

    /// Record one scan, and return the wait before the next, or `None` when
    /// the bound has been spent.
    fn spent(&mut self) -> Option<Duration> {
        self.made = self.made.saturating_add(1);
        if self.made >= SETTLE_SCANS {
            return None;
        }
        let wait = self.next;
        self.next = self.next.saturating_mul(2).min(MAX_SETTLE_DELAY);
        Some(wait)
    }
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

/// The fields of an issue a read-back compares: exactly what
/// [`keyed_projection`] keeps, so no body crosses the capture ceiling.
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

/// The filter that cuts a page of issues to the newest number on it and the
/// issues carrying `key`, newest first, projected.
fn listing_filter(key: &str) -> String {
    format!(
        "{{newest: ([.[].number] | max // 0), keyed: [.[] | {} | select(.keyed)]}}",
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
    /// One listing read: the newest number the repository lists and, of its
    /// newest [`MAX_ISSUES_READ_BACK`] issues, the ones carrying `key`.
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
        if listing.keyed.len() > MAX_ISSUES_READ_BACK {
            let refusal = Err(GhError::Response {
                path,
                reason: format!(
                    "{} issues answered a read of at most {MAX_ISSUES_READ_BACK}",
                    listing.keyed.len()
                ),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "listing: returning an error to the caller");
            return refusal;
        }
        Ok(listing)
    }

    /// Issue `number` as `key`'s scan sees it, or `None` when GitHub has no
    /// such number yet.
    ///
    /// A `404` right after a listing this credential read is the end of the
    /// repository's numbers, not a permission loss. A `410` is a deleted
    /// issue: the number is spent and carries no key, so it reads as an issue
    /// that is not this key's.
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
    /// the one the key was opened as. When the listing holds no match, every
    /// number past its newest is read in turn until GitHub has no such number,
    /// which is the whole of what the listing had not caught up on.
    ///
    /// A read that fails is returned rather than read past: a create issued
    /// over a window nobody could read is the duplicate this scan exists to
    /// prevent, so a failed scan fails the run and creates nothing.
    async fn issue_keyed(&self, draft: &IssueDraft) -> Result<Option<IssueRecord>, GhError> {
        let listing = self.listing(draft.key()).await?;
        if let Some(listed) = listing.keyed.into_iter().last() {
            return listed.for_draft(draft).map(Some);
        }
        let mut number = listing.newest;
        for _ in 0..MAX_UNLISTED_READS {
            number = number.saturating_add(1);
            match self.numbered(number, draft.key()).await? {
                None => return Ok(None),
                Some(unlisted) if unlisted.keyed => return unlisted.for_draft(draft).map(Some),
                Some(_) => {}
            }
        }
        let refusal = Err(GhError::Response {
            path: format!("repos/{}/issues", self.repository.as_str()),
            reason: format!(
                "more than {MAX_UNLISTED_READS} issues exist past #{} the listing reached, so the \
                 scan could not read the window a create would land in",
                listing.newest
            ),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "issue_keyed: returning an error to the caller");
        refusal
    }

    /// The key's issue, scanned for over the settle bound when the first scan
    /// finds nothing.
    ///
    /// The read-back after a create whose answer was lost, and only that: the
    /// create may still be landing, so one scan that misses it is not yet an
    /// answer. The waits are spent on `clock`, the flow's own: a wall clock
    /// waits the real schedule, and a caller-advanceable clock spends the same
    /// schedule without waiting for it.
    async fn settled_keyed(
        &self,
        draft: &IssueDraft,
        clock: &Clock,
    ) -> Result<Option<IssueRecord>, GhError> {
        let mut settle = Settle::new();
        loop {
            if let Some(found) = self.issue_keyed(draft).await? {
                return Ok(Some(found));
            }
            match settle.spent() {
                Some(wait) => clock.sleep(wait).await,
                None => return Ok(None),
            }
        }
    }

    /// Create `draft` once, through a staged `--input` payload, and confirm it
    /// from GitHub's answer.
    ///
    /// The `201` answer is the issue as GitHub stored it, so its number, its
    /// title and the trailer in its body are read from that answer; an answer
    /// that does not show all three is not a confirmation, and the caller reads
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

/// Whether a failed create is known to have created nothing, so no read-back
/// is owed: it never started, GitHub refused the credential, or GitHub
/// rejected the request itself.
const fn created_nothing(failure: &GhError) -> bool {
    matches!(
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
    )
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
    pub fn from_target(target: &str) -> Result<Action, BotError> {
        let repository = Repository::new(target).map_err(|refused| BotError::IncompleteSpec {
            field: "target",
            cause: refused.to_string(),
        })?;
        Ok(Action::new(Self::new(Gh::new(repository))))
    }

    /// Find the key's issue, or create it.
    ///
    /// The scan before the create reads the listing and every number past it,
    /// so a rerun after a lost answer finds the earlier issue however far the
    /// listing lags, without waiting for it. A create whose answer arrives is
    /// confirmed from that answer and never repeated; one whose answer was lost
    /// is read back over the settle bound.
    async fn open(&self, draft: &IssueDraft, clock: &Clock) -> Result<OpenedIssue, BotError> {
        let fail = |failure: GhError| failure.into_bot_error(DOMAIN);
        if let Some(found) = self.gh.issue_keyed(draft).await.map_err(fail)? {
            return Ok(found.settled(Settlement::Found));
        }
        let lost = match self.gh.create_issue(draft).await {
            Ok(created) => return Ok(created.settled(Settlement::Created)),
            Err(failure) if created_nothing(&failure) => {
                lgwks_std::trace::debug!(error = %failure, key = draft.key(), "gh::issue: the create was refused before anything landed");
                return Err(fail(failure));
            }
            Err(failure) => failure,
        };
        // The create ran and its answer is not one: the issue may exist, and
        // may still be landing, so the read-back settles.
        match self.gh.settled_keyed(draft, clock).await {
            Ok(Some(found)) => Ok(found.settled(Settlement::Reconciled)),
            Ok(None) => Err(BotError::EffectIndeterminate {
                domain: DOMAIN.to_owned(),
                cause: format!(
                    "{lost}; the read-back did not find key {}, so the issue may or may not \
                     exist, and a rerun with this key reads back before it creates",
                    draft.key()
                ),
            }),
            Err(unread) => Err(BotError::EffectIndeterminate {
                domain: DOMAIN.to_owned(),
                cause: format!("{lost}; the read-back failed too: {unread}"),
            }),
        }
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
        // clock, which is the live schedule a real GitHub lag needs.
        self.execute_action_on(&Clock::wall(), call).await
    }

    async fn execute_action_on(
        &self,
        clock: &Clock,
        call: (Auth, &IssueDraft),
    ) -> Result<OpenedIssue, BotError> {
        // Checked before `gh` starts, so a denial created and read nothing.
        call.0.check(self.required_caps())?;
        self.open(call.1, clock).await
    }

    fn domain_id(&self) -> &str {
        DOMAIN
    }
}
