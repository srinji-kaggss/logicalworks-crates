// File: crates/lgwks-bot/src/domain/gh.rs (rust)
//! `gh` owns the GitHub domain through the `gh` command-line client.
//!
//! Every call is one supervised child process: the GitHub CLI is the only
//! handle this crate has on GitHub, so it is admitted as a
//! [`ProcessSpec`](crate::rt::process::ProcessSpec) and run through
//! [`Supervisor::run_process`](crate::rt::supervise::Supervisor::run_process),
//! which owns the process group, the bounded capture and the deadline. Nothing
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
//!   bound.
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
            return Err(invalid("the repository reference is empty"));
        }
        if spec.len() > MAX_REPOSITORY_BYTES {
            return Err(invalid("the repository reference is too long"));
        }
        let mut parts = spec.split('/');
        let owner = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        if parts.next().is_some() {
            return Err(invalid(
                "a repository reference is `owner/repo`, with exactly one `/`",
            ));
        }
        if owner.is_empty() || name.is_empty() {
            return Err(invalid("both `owner` and `repo` must be non-empty"));
        }
        let allowed = |segment: &str| {
            segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        };
        if !allowed(owner) || !allowed(name) {
            return Err(invalid(
                "`owner` and `repo` hold ASCII letters, digits, '-', '_' and '.' only",
            ));
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
            return Err(GhError::CommitId {
                hex,
                reason: "a commit id is 40 hexadecimal characters",
            });
        }
        if !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(GhError::CommitId {
                hex,
                reason: "a commit id is lowercase hexadecimal",
            });
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
            user: None,
        }
    }

    /// Whether this record is the review `intended` describes.
    ///
    /// The subject, the body and the state are all compared, because a
    /// read-back that matched only the subject would accept a review of the
    /// right commit saying something else — which is not verification of this
    /// publication. The marker is deliberately **not** compared: it locates a
    /// candidate, and treating it as proof is exactly the mistake PR-07 names.
    #[must_use]
    pub fn matches(&self, intended: &ReviewPayload) -> bool {
        self.commit_id.as_deref() == Some(intended.commit_id.as_str())
            && self.body.as_deref() == Some(intended.body.as_str())
            && self.state.eq_ignore_ascii_case(intended.event.as_str())
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

/// One pull request's pinned head, as `gh api` reports it.
///
/// `#[non_exhaustive]`: GitHub adds fields to this object, and a consumer that
/// destructured it literally would break on each addition.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(crate = "lgwks_std::json::serde")]
#[non_exhaustive]
pub struct PrSnapshot {
    /// The pull request number.
    #[serde(default)]
    number: u64,
    /// The head commit's sha at the moment of the read.
    #[serde(default)]
    head_sha: String,
    /// The base commit's sha at the moment of the read.
    #[serde(default)]
    base_sha: String,
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
    /// The repository this names.
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
    /// The top-level body.
    body: String,
    /// An application marker that helps a read-back locate a candidate review.
    ///
    /// A locator, never a proof: [`ReviewRecord::matches`] verifies subject,
    /// body and state before a publication is reported.
    marker: String,
}

impl ReviewPayload {
    /// The commit this review is about.
    #[must_use]
    pub fn commit_id(&self) -> &str {
        &self.commit_id
    }

    /// The review event.
    #[must_use]
    pub fn event(&self) -> &str {
        &self.event
    }

    /// The top-level body.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// The application marker.
    #[must_use]
    pub fn marker(&self) -> &str {
        &self.marker
    }
}

impl ReviewPayload {
    /// The review events GitHub accepts.
    pub const EVENTS: [&'static str; 3] = ["COMMENT", "APPROVE", "REQUEST_CHANGES"];

    /// Build a payload bound to `subject`.
    ///
    /// # Errors
    ///
    /// [`GhError::Event`] naming the events that are accepted, when `event` is
    /// not one of them.
    pub fn new(
        subject: &CommitId,
        event: impl Into<String>,
        body: impl Into<String>,
        marker: impl Into<String>,
    ) -> Result<Self, GhError> {
        let event = event.into();
        if !Self::EVENTS.contains(&event.as_str()) {
            return Err(GhError::Event { event });
        }
        Ok(Self {
            commit_id: subject.as_str().to_owned(),
            event,
            body: body.into(),
            marker: marker.into(),
        })
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
            return Err(GhError::TruncatedResponse {
                retained: self.stdout.len(),
                total: self.stdout_total_bytes,
            });
        }
        json::from_str::<T>(self.stdout.trim()).map_err(|source| GhError::MalformedResponse {
            path: String::from("stdout"),
            source: source.to_string(),
        })
    }
}

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

    /// Read the pinned head and base of `pull`.
    ///
    /// # Errors
    ///
    /// [`GhError::Process`] when the client could not be run at all,
    /// [`GhError::Transport`] naming the exit code or the deadline when it ran
    /// and failed, and [`GhError::Response`] when it answered with something
    /// that is not a pull request.
    /// Without the `process` feature there is no runner, and saying so is the
    /// honest answer: a binding that cannot reach GitHub must not report an
    /// empty answer, which a caller could mistake for "GitHub has no reviews".
    ///
    /// # Errors
    ///
    /// [`GhError::NoRunner`], always.
    #[cfg(not(feature = "process"))]
    pub async fn snapshot(&self, pull: &PullRequest) -> Result<PrSnapshot, GhError> {
        let path = format!("repos/{}/pulls/{}", pull.repository(), pull.number());
        let args = self.args_for(&["--method", "GET", &path]);
        self.call(&args).await.map(|_| PrSnapshot::default())
    }

    /// Read the pinned head and base of `pull`.
    ///
    /// # Errors
    ///
    /// [`GhError::Process`] when the client could not be run at all,
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
        if snapshot.head_sha().len() != SHA_HEX_LEN || snapshot.base_sha().len() != SHA_HEX_LEN {
            return Err(GhError::Response {
                path,
                reason: String::from("the pull request has no 40-character head and base"),
            });
        }
        Ok(snapshot)
    }

    /// Read every review currently on `pull`.
    ///
    /// The independent observation a publication is verified against: it is a
    /// separate process from the one that wrote, and it reads GitHub's own
    /// record rather than the exit code of the write.
    ///
    /// # Errors
    ///
    /// As [`Gh::snapshot`], plus [`GhError::Response`] when the answer is not
    /// a review list.
    ///
    /// Without the `process` feature there is no runner, so no review is read
    /// and none is reported as absent.
    ///
    /// # Errors
    ///
    /// [`GhError::NoRunner`], always.
    #[cfg(not(feature = "process"))]
    pub async fn read_reviews(&self, pull: &PullRequest) -> Result<Vec<ReviewRecord>, GhError> {
        let path = format!(
            "repos/{}/pulls/{}/reviews",
            pull.repository(),
            pull.number()
        );
        let args = self.args_for(&["--method", "GET", &path]);
        self.call(&args).await.map(|_| Vec::new())
    }

    /// Read every review currently on `pull`, through a real supervised call.
    ///
    /// # Errors
    ///
    /// As [`Gh::snapshot`], plus [`GhError::Response`] when the answer is not
    /// a review list.
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
        outcome.parse_json::<Vec<ReviewRecord>>()
    }

    /// Without the `process` feature there is no runner, so nothing is
    /// published and nothing is reported as published.
    ///
    /// # Errors
    ///
    /// [`GhError::NoRunner`], always.
    #[cfg(not(feature = "process"))]
    pub async fn publish(
        &self,
        pull: &PullRequest,
        _payload: &ReviewPayload,
    ) -> Result<u64, GhError> {
        let path = format!(
            "repos/{}/pulls/{}/reviews",
            pull.repository(),
            pull.number()
        );
        let args = self.args_for(&["--method", "POST", &path]);
        self.call(&args).await.map(|_| 0)
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
            return Err(GhError::Response {
                path,
                reason: String::from("the created review has no id"),
            });
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
            return Err(GhError::Deadline {
                what: what.to_owned(),
            });
        }
        match self.exit_code {
            Some(0) => Ok(()),
            Some(code) => Err(GhError::Transport {
                what: what.to_owned(),
                exit_code: Some(code),
                stderr: self.stderr.clone(),
            }),
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
fn unique_tag() -> String {
    #[cfg(feature = "ephemeral")]
    {
        let bytes = lgwks_std::random::bytes::<8>().unwrap_or([0; 8]);
        bytes.iter().fold(String::new(), |mut text, byte| {
            use std::fmt::Write as _;
            // A `write!` into a `String` is infallible; the result is bound
            // rather than discarded so nothing in this crate can ignore a
            // formatting failure by accident.
            let _written = write!(text, "{byte:02x}");
            text
        })
    }
    // Without the `ephemeral` feature this crate may not assume an entropy
    // source (INV-DEP-6), so there is no tag to give. Rather than fall back to
    // something the OS reuses — which is how two concurrent publications end
    // up refusing each other's staged file — the publish path says it cannot
    // run, and the review task reports that rather than publishing under a name
    // it cannot guarantee. A `Refused` is the honest answer here.
    #[cfg(not(feature = "ephemeral"))]
    {
        String::new()
    }
}

/// A payload staged on disk for one `gh --input` call.
///
/// The alternative is a second process path with a piped stdin, and the crate
/// has exactly one sanctioned runner. A staged file is a resource with a known
/// lifetime and an owner, which is the shape the estate's rules already
/// require; a private spawner would be neither.
#[cfg(feature = "process")]
struct StagedInput(std::path::PathBuf);

#[cfg(feature = "process")]
impl StagedInput {
    /// The path the child is told to read.
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    /// The path as an argument, which is a `String` the `ProcessSpec` takes.
    ///
    /// Returned by reference so the argument vector is built once and the
    /// staged file stays owned by exactly one value — the one whose `Drop`
    /// removes it.
    fn path_str(&self) -> &str {
        // A temp path on every supported platform is built from UTF-8
        // components: `std::env::temp_dir` is documented to return a path, and
        // this adapter refuses a repository, a commit and an event as typed
        // values precisely so that nothing unvalidated reaches the command
        // line. A non-UTF-8 temporary directory is the one input that is not
        // ours, so it is refused rather than lossily converted into a path that
        // names a different file than the one that was written.
        self.path().to_str().unwrap_or_default()
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
                path = %self.0.display(),
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
    let tag = unique_tag();
    if tag.is_empty() {
        return Err(GhError::NoRunner);
    }
    path.push(format!("lgwks-gh-payload-{tag}.json"));
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
    Ok(StagedInput(path))
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
    #[must_use]
    pub const fn is_read_only_retryable(&self) -> bool {
        matches!(
            *self,
            Self::Deadline { .. } | Self::MalformedResponse { .. } | Self::Transport { .. }
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
            | Self::Transport { .. }
            | Self::Deadline { .. }
            | Self::Response { .. }
            | Self::TruncatedResponse { .. }
            | Self::MalformedResponse { .. }
            | Self::PayloadNotSent { .. }
            | Self::Staging { .. } => None,
        }
    }
}

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
