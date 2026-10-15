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
//! - **Capability is checked before the fork.** Every verb takes an [`Auth`](crate::cap::Auth)
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

#[cfg(feature = "process")]
use lgwks_std::json;

use crate::cap::Cap;

#[cfg(feature = "process")]
use super::sys::{DEFAULT_CAPTURE_LIMIT, DEFAULT_DEADLINE};

#[cfg(feature = "process")]
use crate::rt::process::{ProcessRun, ProcessSpec};
#[cfg(feature = "process")]
use crate::rt::supervise::Supervisor;

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

// Identifiers, the read-back record, what a call sends and returns, the
// refusal vocabulary and the verbs, each in its own file; re-exported so every
// path stays `domain::gh::<Name>`.
mod error;
mod ids;
mod payload;
mod record;
mod verbs;

#[cfg(feature = "process")]
use record::MovedWire;

pub use error::GhError;
pub use ids::{CommitId, MAX_REPOSITORY_BYTES, Repository, SHA_HEX_LEN};
pub use payload::{
    GhOutcome, MAX_DIFF_BYTES, MAX_DIFF_FILES_PER_PULL, MAX_REVIEWS_PER_PULL, PullRequest,
    ReviewPayload,
};
pub use record::{
    ChangedFile, PrSnapshot, PullDiff, REVIEW_STATE_PENDING, ReviewAuthor, ReviewComment,
    ReviewRecord,
};
pub use verbs::{GhQuery, PrSnapshotSource};
