//! `sys` owns the system process domain. Requires `bot.sys`.
//!
//! With the `process` feature the domain runs a real child through
//! `Supervisor::run_process` and reports what it observed: exit code or signal,
//! captured stdout and stderr with their truncation accounting, whether a
//! deadline stopped it, and the process-group cleanup receipt. Without the
//! feature the domain keeps its refusal: there is no runner to bind to, and a
//! process-control capability that cannot run a process reports that rather
//! than pretending.
//!
//! Three facts this domain is careful to report truthfully:
//!
//! - **Exit zero is an exit code, not a completed task.** [`ProcessState`]
//!   carries the code; whether `0` means the work succeeded is the caller's
//!   judgement (T35).
//! - **A deadline is a stop, not an exit.** The state's `deadline_fired` is set
//!   and the supervisor has reaped the whole group.
//! - **Dispatch certainty is not a single answer.** A refusal before the fork
//!   is [`DispatchCertainty::Refused`] — nothing ran; a failure after the child
//!   started is [`BotError::EffectIndeterminate`] — it ran and the outcome is
//!   unknown.

use crate::cap::{Auth, Cap};
use crate::error::{BotError, DispatchCertainty};
use crate::verb;

#[cfg(feature = "process")]
use crate::rt::process::{Frames, ProcessRun, ProcessRunError, ProcessSpec};
#[cfg(feature = "process")]
use crate::rt::supervise::Supervisor;
#[cfg(feature = "process")]
use crate::rt::sync::Semaphore;
#[cfg(feature = "process")]
use std::num::NonZeroUsize;
#[cfg(feature = "process")]
use std::time::Duration;

/// The default retained-byte ceiling for each captured stream of a [`Process`]
/// built with [`Process::new`].
///
/// Finite by design: a one-shot process that writes without bound cannot make
/// the bot retain without bound. The first 64 KiB of each stream is kept; the
/// rest is drained and counted, never retained.
#[cfg(feature = "process")]
pub const DEFAULT_CAPTURE_LIMIT: NonZeroUsize = match NonZeroUsize::new(64 * 1024) {
    Some(limit) => limit,
    None => NonZeroUsize::MIN,
};

/// The default maximum runtime for a [`Process`] built with [`Process::new`].
///
/// Finite by design: a process that never exits is stopped, as a whole group,
/// and reported as a deadline. Half a minute is long enough for ordinary work
/// and short enough that a stuck child is not a stuck bot.
#[cfg(feature = "process")]
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(30);

/// The default number of children one [`Process`] runs at once.
///
/// Finite by design: every verb call forks, and the bot polls and executes
/// concurrently, so without a ceiling a burst of calls is a burst of children.
/// Calls past the ceiling wait for a slot rather than forking.
#[cfg(feature = "process")]
pub const DEFAULT_MAX_CONCURRENT: NonZeroUsize = match NonZeroUsize::new(16) {
    Some(limit) => limit,
    None => NonZeroUsize::MIN,
};

/// Observe or execute a system process. Supports Observe, Execute, Query.
///
/// A one-shot command is run to completion on each call. `Execute` is the verb
/// for a command with an effect; `Query` is the read-only probe form; `Observe`
/// re-runs the command each tick and returns the state, which the ECS schedule
/// treats as change detection — a probe whose reported state moves fires the
/// chain. All three run the same validated [`ProcessSpec`] through the
/// supervisor, so none of them is a silent no-op.
#[cfg(feature = "process")]
#[derive(Debug)]
pub struct Process {
    /// The validated description of what to run.
    spec: ProcessSpec,
    /// Forced to `[bot.sys]` by the constructor: process control is the
    /// capability this domain exists to gate, and no constructor omits it.
    caps: Vec<Cap>,
    /// One slot per child this domain may run at once, shared by every verb
    /// call on this value, so concurrent calls cannot fork past the ceiling.
    slots: Semaphore,
    /// The reader ceiling for the framed stdout reading, when the caller asked
    /// for one. `None` (the default) leaves [`ProcessState::stdout_frames`]
    /// empty and reports stdout exactly as before; setting it reaches the
    /// crate's one frame grammar through the verb path.
    frame_stdout: Option<NonZeroUsize>,
}

/// Observe or execute a system process. Supports Observe, Execute, Query.
///
/// Built without the `process` feature there is no supervised runner to bind
/// to, so every verb refuses.
#[cfg(not(feature = "process"))]
#[derive(Debug)]
pub struct Process {
    /// The command name. Retained for the "binding required" diagnostic; the
    /// domain performs no parsing of it.
    command: String,
    /// Forced to `[bot.sys]` by the constructor: process control is the
    /// capability this domain exists to gate, and no constructor omits it.
    caps: Vec<Cap>,
}

/// Process state returned by observation, execution, or query.
///
/// `#[non_exhaustive]`: the reported shape grows with the domain, and a
/// consumer that destructured this literally would break on each addition.
///
/// Every field is a fact about the run, never a verdict about the work: an
/// `exit_code` of `Some(0)` says the command exited zero, not that the task
/// succeeded, and `deadline_fired` says the supervisor stopped it.
#[derive(PartialEq, Debug, Clone)]
#[non_exhaustive]
pub struct ProcessState {
    /// Whether the process is running. Always `false` for a one-shot run that
    /// has returned.
    pub running: bool,
    /// Exit code if the process exited normally.
    pub exit_code: Option<i32>,
    /// Standard output, lossily decoded from the retained bytes.
    ///
    /// Private with an accessor because it is mutable state behind the
    /// invariant that it is exactly what the process wrote: a caller reads it,
    /// never edits it in place.
    stdout: String,
    /// Standard error, lossily decoded from the retained bytes. See [`stdout`].
    ///
    /// [`stdout`]: ProcessState::stdout
    stderr: String,
    /// Whether stdout held more than the retained ceiling.
    pub stdout_truncated: bool,
    /// Whether stderr held more than the retained ceiling.
    pub stderr_truncated: bool,
    /// Every byte the child wrote to stdout, retained or not.
    pub stdout_total_bytes: u64,
    /// Every byte the child wrote to stderr, retained or not.
    pub stderr_total_bytes: u64,
    /// Whether the supervisor stopped the process because its deadline elapsed.
    pub deadline_fired: bool,
    /// The terminating signal, when the process was killed by one.
    pub signal: Option<i32>,
    /// The length-framed reading of stdout, when the domain was built with
    /// [`Process::frame_stdout`].
    ///
    /// Private with an accessor because it is a reading of bytes the process
    /// wrote: a caller reads it, never edits it in place. `None` for a domain
    /// built without a framed reading, which is the default.
    #[cfg(feature = "process")]
    stdout_frames: Option<Frames>,
}

impl ProcessState {
    /// Standard output, lossily decoded from the retained bytes.
    #[must_use]
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    /// Standard error, lossily decoded from the retained bytes.
    #[must_use]
    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    /// The child's stdout read as length-framed records, when the domain was
    /// built with [`Process::frame_stdout`].
    ///
    /// `None` for a domain built without it, which is the default. The reading
    /// is byte-exact where [`ProcessState::stdout`] is lossy, and its ending is
    /// the one [`crate::rt::process::CapturedStream::frames`] decides for a
    /// capture that may have cut the child's stream.
    #[cfg(feature = "process")]
    #[must_use]
    pub fn stdout_frames(&self) -> Option<&Frames> {
        self.stdout_frames.as_ref()
    }
}

#[cfg(feature = "process")]
impl Process {
    /// Create a system process observer/executor from a program name.
    ///
    /// The command runs as a program with no arguments; the default capture
    /// ceiling ([`DEFAULT_CAPTURE_LIMIT`]) and default deadline
    /// ([`DEFAULT_DEADLINE`]) are applied, so the state this domain returns is
    /// bounded even when the program is not. Use [`Process::from_spec`] to give
    /// arguments, environment, or different bounds.
    pub fn new(command: impl Into<String>) -> Self {
        let mut spec = ProcessSpec::new(command.into());
        spec.capture_stdout(DEFAULT_CAPTURE_LIMIT);
        spec.capture_stderr(DEFAULT_CAPTURE_LIMIT);
        spec.deadline(DEFAULT_DEADLINE);
        Self::from_spec(spec)
    }

    /// Create a process domain from a validated [`ProcessSpec`].
    ///
    /// The spec's own stream policies and deadline govern; nothing is defaulted
    /// here, so a spec that captures nothing inherits the streams and one
    /// without a deadline runs until the supervisor is cancelled. At most
    /// [`DEFAULT_MAX_CONCURRENT`] children run at once; see
    /// [`Process::max_concurrent`].
    #[must_use]
    pub fn from_spec(spec: ProcessSpec) -> Self {
        Self {
            spec,
            caps: vec![Cap::sys()],
            slots: Semaphore::new(DEFAULT_MAX_CONCURRENT.get()),
            frame_stdout: None,
        }
    }

    /// Attach a length-framed reading of the child's stdout.
    ///
    /// When set, every [`ProcessState`] the verbs return carries the child's
    /// stdout read as length-prefixed records under `ceiling` through
    /// [`ProcessState::stdout_frames`] — the crate's one frame grammar
    /// ([`crate::journal::frame`]) reached from the verb a caller performs work
    /// with. The reading is byte-exact, so a binary record round-trips where
    /// the lossy [`ProcessState::stdout`] cannot. Without it the framed reading
    /// is absent and nothing else about the run changes.
    #[must_use]
    pub fn frame_stdout(mut self, ceiling: NonZeroUsize) -> Self {
        self.frame_stdout = Some(ceiling);
        self
    }

    /// Set how many children this domain runs at once.
    ///
    /// Every verb call on this value claims one slot before it forks and holds
    /// it until the child is reaped, so a burst of concurrent calls waits for a
    /// slot instead of starting a burst of children.
    #[must_use]
    pub fn max_concurrent(mut self, limit: NonZeroUsize) -> Self {
        self.slots = Semaphore::new(limit.get());
        self
    }

    /// Run the command once inside this domain's ceiling and map the outcome.
    ///
    /// The slot is claimed before the fork and released when the run ends, and
    /// a dropped call releases it with the run, so the ceiling cannot leak.
    /// The three error arms are the whole retry contract: a refusal or a failed
    /// start established that nothing ran, so the certainty is
    /// [`DispatchCertainty::Refused`]; a failure after the child started
    /// established that it did run, so it is [`BotError::EffectIndeterminate`]
    /// and never `Refused`.
    async fn run_once(&self) -> Result<ProcessState, BotError> {
        let Ok(_slot) = self.slots.acquire().await else {
            return Err(BotError::DomainError {
                domain: String::from("sys::process"),
                certainty: DispatchCertainty::Refused,
                cause: String::from("the process slots were closed before the process started"),
            });
        };
        let mut supervisor = Supervisor::new(1);
        match supervisor.run_process(&self.spec).await {
            Ok(run) => Ok(ProcessState::from_run(&run, self.frame_stdout)),
            Err(ProcessRunError::Refused) => Err(BotError::DomainError {
                domain: String::from("sys::process"),
                certainty: DispatchCertainty::Refused,
                cause: String::from("the supervisor was cancelled before the process started"),
            }),
            Err(ProcessRunError::NotStarted { source }) => Err(BotError::DomainError {
                domain: String::from("sys::process"),
                certainty: DispatchCertainty::Refused,
                cause: format!("the process did not start: {source}"),
            }),
            Err(ProcessRunError::AfterStart { source }) => Err(BotError::EffectIndeterminate {
                domain: String::from("sys::process"),
                cause: format!("the process started but its outcome is unknown: {source}"),
            }),
        }
    }
}

#[cfg(not(feature = "process"))]
impl Process {
    /// Create a system process observer/executor.
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            caps: vec![Cap::sys()],
        }
    }
}

#[cfg(feature = "process")]
impl ProcessState {
    /// Fold one supervised run into the observable state.
    ///
    /// `frame_stdout` is the domain's own reader ceiling: when present, the
    /// same bytes `stdout` decodes lossily are read once through the crate's
    /// frame grammar, so the framed reading has a production caller rather than
    /// existing only for a test.
    fn from_run(run: &ProcessRun, frame_stdout: Option<NonZeroUsize>) -> Self {
        let stdout = run.stdout();
        let stderr = run.stderr();
        #[cfg(unix)]
        let signal = run.signal();
        #[cfg(not(unix))]
        let signal: Option<i32> = None;
        Self {
            running: false,
            exit_code: run.exit_code(),
            stdout: String::from_utf8_lossy(stdout.bytes()).into_owned(),
            stderr: String::from_utf8_lossy(stderr.bytes()).into_owned(),
            stdout_truncated: stdout.truncated(),
            stderr_truncated: stderr.truncated(),
            stdout_total_bytes: stdout.total_bytes(),
            stderr_total_bytes: stderr.total_bytes(),
            deadline_fired: run.deadline_fired(),
            signal,
            stdout_frames: frame_stdout.map(|ceiling| stdout.frames(ceiling.get())),
        }
    }
}

impl Process {
    /// Run one verb call, or refuse it when no supervised runner is bound.
    ///
    /// One body for the three verbs and both feature sets, so a refusal message
    /// and the successful path cannot drift between them. What differs between
    /// a build with the `process` runner and one without is a fact about this
    /// method rather than about each verb: a build with no runner has no
    /// supervised process to bind to, so the refusal names the verb and the
    /// command it was asked to run.
    async fn dispatch(&self, verb: &'static str) -> Result<ProcessState, BotError> {
        #[cfg(feature = "process")]
        {
            let _ = verb;
            self.run_once().await
        }
        #[cfg(not(feature = "process"))]
        {
            Err(BotError::DomainError {
                domain: String::from("sys::process"),
                certainty: DispatchCertainty::Refused,
                cause: format!("{verb} {:?} — binding required", self.command),
            })
        }
    }
}

impl verb::Observe for Process {
    type Output = ProcessState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<ProcessState, BotError> {
        call.0.check(self.required_caps())?;
        // A one-shot process has no persistent state to read, so the observed
        // value is the result of running it: the ECS schedule fires the chain
        // when that state moves.
        self.dispatch("polling").await
    }

    fn domain_id(&self) -> &str {
        "sys::process"
    }
}

impl verb::Execute for Process {
    type Input = ();
    type Output = ProcessState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn execute_action(&self, call: (Auth, &())) -> Result<ProcessState, BotError> {
        call.0.check(self.required_caps())?;
        self.dispatch("executing").await
    }

    fn domain_id(&self) -> &str {
        "sys::process"
    }
}

impl verb::Query for Process {
    type Input = ();
    type Output = ProcessState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn query(&self, call: (Auth, &())) -> Result<ProcessState, BotError> {
        call.0.check(self.required_caps())?;
        // The read-only probe form: it runs the same command and reports, and
        // by construction performs no write of its own beyond what the command
        // does. A command with an effect belongs in `Execute`.
        self.dispatch("querying").await
    }

    fn domain_id(&self) -> &str {
        "sys::process"
    }
}
