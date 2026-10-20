//! `gh::issue`: open one GitHub issue per caller key, settled by reading
//! GitHub back rather than by trusting a create's answer.
//!
//! GitHub's create-issue request has no idempotency key, so the key travels in
//! the issue itself, as an HTML comment trailer (`<!-- lgwks-issue:KEY -->`)
//! that renders as nothing. Every open reads the repository's newest
//! [`MAX_ISSUES_READ_BACK`] issues first, so a second run with the same key —
//! a rerun, a resume after a kill, a retry after a lost answer — finds the
//! first run's issue instead of opening another. A create whose answer was
//! lost is reconciled by one more read; one that still cannot be found is
//! [`BotError::EffectIndeterminate`], never a second create.
//!
//! GitHub's listing lags a create by seconds, so a create is confirmed at the
//! number GitHub answered and then reported only once the listing shows it;
//! until it does, a rerun could not find it.
//!
//! The bounds are stated rather than hidden: an issue older than the newest
//! [`MAX_ISSUES_READ_BACK`] in its repository is not found, a key reused after
//! that many newer issues opens a new one, and a create whose answer was lost
//! inside the listing's lag can be missed by an immediate rerun (#658).

use std::time::Duration;

use lgwks_std::json::{self, Deserialize, Serialize};

use super::{Gh, GhError, Repository, stage_input};
use crate::Action;
use crate::cap::{Auth, Cap};
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
    /// This run created it, and the read-back shows it.
    Created,
    /// An earlier run with this key had opened it; nothing was created.
    Found,
    /// This run's create lost its answer, and the read-back found the issue.
    Reconciled,
    /// This run created it and read it back at its number, but the listing a
    /// rerun's key scan reads had not shown it when the settle bound ran out:
    /// a rerun before it is listed may open a second issue.
    Unlisted,
}

impl Settlement {
    /// The lowercase word a report prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Found => "found",
            Self::Reconciled => "reconciled",
            Self::Unlisted => "unlisted",
        }
    }
}

/// How many listing reads a create waits through for its issue to be listed.
const SETTLE_POLLS: u8 = 8;

/// The wait before the second listing read; each later wait doubles.
const FIRST_SETTLE_DELAY: Duration = Duration::from_millis(250);

/// The longest single wait between listing reads; eight reads span about 20 s.
const MAX_SETTLE_DELAY: Duration = Duration::from_secs(4);

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

/// The fields of an issue a read-back compares: exactly what [`PROJECTION`]
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
}

/// The `gh --jq` projection every answer is cut to before it is printed.
///
/// GitHub answers an issue with its whole body, and a hundred bodies run to
/// megabytes, past any capture a supervised child is given (a live read of
/// this repository's newest 100 issues was 567,275 bytes against a 65,536-byte
/// ceiling). `gh` evaluates the filter in its own process, so what is captured
/// is three fields per issue, whatever the bodies hold.
const PROJECTION: &str = "{number, html_url, title}";

/// The filter that keeps, of a page of issues, the ones carrying `key`'s
/// trailer, newest first, projected.
///
/// Pull requests share the issues endpoint and are dropped: a key names an
/// issue. The key's alphabet (`[A-Za-z0-9._:-]`, enforced by
/// [`IssueDraft::new`]) holds no quote or backslash, so it is a literal inside
/// the filter's string.
fn keyed_filter(key: &str) -> String {
    format!(
        "[.[] | select(.pull_request == null) | select((.body // \"\") | contains(\"{}\")) | {PROJECTION}]",
        trailer(key)
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
}

impl Gh {
    /// Of the repository's newest [`MAX_ISSUES_READ_BACK`] issues, the ones
    /// carrying `key`, newest first, filtered inside `gh`.
    async fn newest_keyed(&self, key: &str) -> Result<Vec<IssueRecord>, GhError> {
        let path = format!(
            "repos/{}/issues?state=all&sort=created&direction=desc&per_page={MAX_ISSUES_READ_BACK}",
            self.repository.as_str()
        );
        let filter = keyed_filter(key);
        let args = self.args_for(&["--method", "GET", &path, "--jq", &filter]);
        let outcome = self.call(&args).await?;
        outcome.require_success("reading the repository's newest issues")?;
        let issues = outcome.parse_json::<Vec<IssueRecord>>()?;
        if issues.len() > MAX_ISSUES_READ_BACK {
            let refusal = Err(GhError::Response {
                path,
                reason: format!(
                    "{} issues answered a read of at most {MAX_ISSUES_READ_BACK}",
                    issues.len()
                ),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "newest_keyed: returning an error to the caller");
            return refusal;
        }
        Ok(issues)
    }

    /// The first issue opened under `draft`'s key, when one is in the window.
    ///
    /// The oldest match, not the newest: when a key ever names two issues (the
    /// live duplicate #659 beside #658), every later run answers the one the
    /// key was opened as, never the duplicate.
    ///
    /// A key found on an issue with another title is refused rather than
    /// adopted: one key names one issue, and handing back an issue about
    /// something else would report an open that never happened.
    async fn issue_keyed(&self, draft: &IssueDraft) -> Result<Option<IssueRecord>, GhError> {
        match self.newest_keyed(draft.key()).await?.into_iter().last() {
            Some(issue) if issue.title != draft.title() => {
                let refusal = Err(GhError::Response {
                    path: issue.html_url,
                    reason: format!(
                        "the key {} already names issue #{} with another title; a key names one issue",
                        draft.key(),
                        issue.number
                    ),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "issue_keyed: returning an error to the caller");
                refusal
            }
            other => Ok(other),
        }
    }

    /// Issue `number`, when it carries `draft`'s key and title.
    ///
    /// The read that confirms a create. GitHub's listing lags a create by
    /// seconds (the first live open, #657, was missing from the newest-issues
    /// read made right after GitHub answered it), while the issue's own address
    /// answers at once; so the create is confirmed where it was answered, and
    /// the key scan is left to the reads that have no number to ask for.
    async fn issue_numbered(
        &self,
        number: u64,
        draft: &IssueDraft,
    ) -> Result<Option<IssueRecord>, GhError> {
        let path = format!("repos/{}/issues/{number}", self.repository.as_str());
        let filter = format!(
            "if ((.body // \"\") | contains(\"{}\")) then {PROJECTION} else {{number: 0}} end",
            trailer(draft.key())
        );
        let args = self.args_for(&["--method", "GET", &path, "--jq", &filter]);
        let outcome = self.call(&args).await?;
        outcome.require_success("reading the created issue back")?;
        let read = outcome.parse_json::<IssueRecord>()?;
        Ok((read.number == number && read.title == draft.title()).then_some(read))
    }

    /// Create `draft` once, through a staged `--input` payload.
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
        let args = self.args_for(&[
            "--method",
            "POST",
            &path,
            "--input",
            staging.path_str(),
            "--jq",
            PROJECTION,
        ]);
        let outcome = self.run_spec(&args).await?;
        outcome.require_success("opening the issue")?;
        let created = outcome.parse_json::<IssueRecord>()?;
        if created.number == 0 {
            let refusal = Err(GhError::Response {
                path,
                reason: String::from("the created issue has no number"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "create_issue: returning an error to the caller");
            return refusal;
        }
        Ok(created)
    }
}

/// Whether a failed create is known to have created nothing, so no read-back
/// is owed: it never started, or GitHub refused the credential.
const fn created_nothing(failure: &GhError) -> bool {
    matches!(
        *failure,
        GhError::IssueDraft { .. }
            | GhError::PayloadNotSent { .. }
            | GhError::Staging { .. }
            | GhError::NoRunner
            | GhError::Unauthorized { .. }
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

    /// The issue this run created, once the listing shows it.
    ///
    /// A rerun decides "already opened" from the listing, and the listing lags
    /// a create: an immediate live rerun opened #659 beside #658. So a create
    /// is not reported until the listing shows it, which is what makes the
    /// next run's scan find it; [`Settlement::Unlisted`] says the bound ran
    /// out first. A listing read that fails is waited past, not reported: the
    /// issue is already confirmed at its number.
    async fn listed(&self, created: IssueRecord, draft: &IssueDraft) -> OpenedIssue {
        let mut delay = FIRST_SETTLE_DELAY;
        for poll in 1..=SETTLE_POLLS {
            match self.gh.newest_keyed(draft.key()).await {
                Ok(listed) if listed.iter().any(|read| read.number == created.number) => {
                    return created.settled(Settlement::Created);
                }
                Ok(_) => {}
                Err(unread) => {
                    lgwks_std::trace::debug!(error = %unread, poll, "gh::issue: a settle read failed");
                }
            }
            if poll < SETTLE_POLLS {
                crate::rt::time::sleep(delay).await;
                delay = delay.saturating_mul(2).min(MAX_SETTLE_DELAY);
            }
        }
        created.settled(Settlement::Unlisted)
    }

    /// Find the key's issue, or create it and read it back.
    async fn open(&self, draft: &IssueDraft) -> Result<OpenedIssue, BotError> {
        let fail = |failure: GhError| failure.into_bot_error(DOMAIN);
        if let Some(found) = self.gh.issue_keyed(draft).await.map_err(fail)? {
            return Ok(found.settled(Settlement::Found));
        }
        let lost = match self.gh.create_issue(draft).await {
            Ok(created) => {
                return match self.gh.issue_numbered(created.number, draft).await {
                    Ok(Some(read)) => Ok(self.listed(read, draft).await),
                    Ok(None) => Err(BotError::EffectIndeterminate {
                        domain: DOMAIN.to_owned(),
                        cause: format!(
                            "GitHub answered the create with issue #{}, and reading #{} back \
                             does not show key {} and its title",
                            created.number,
                            created.number,
                            draft.key()
                        ),
                    }),
                    Err(unread) => Err(BotError::EffectIndeterminate {
                        domain: DOMAIN.to_owned(),
                        cause: format!(
                            "GitHub answered the create with issue #{}, and reading it back \
                             failed: {unread}",
                            created.number
                        ),
                    }),
                };
            }
            Err(failure) if created_nothing(&failure) => {
                lgwks_std::trace::debug!(error = %failure, key = draft.key(), "gh::issue: the create was refused before anything landed");
                return Err(fail(failure));
            }
            Err(failure) => failure,
        };
        // The create ran and its answer is not one: the issue may exist.
        match self.gh.issue_keyed(draft).await {
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
        // Checked before `gh` starts, so a denial created and read nothing.
        call.0.check(self.required_caps())?;
        self.open(call.1).await
    }

    fn domain_id(&self) -> &str {
        DOMAIN
    }
}
