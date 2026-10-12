//! Non-executable descriptions of supervised child processes.
//!
//! [`ProcessSpec`] is data: it records what the supervisor may run, but it is
//! not an engine command and has no process-control methods. The two execution
//! paths are
//! [`Supervisor::spawn_process`](crate::rt::supervise::Supervisor::spawn_process),
//! which reports the child's outcome later, and
//! [`Supervisor::run_process`](crate::rt::supervise::Supervisor::run_process),
//! which awaits the child and returns its captured output. Both keep the child
//! and its process group under supervisor ownership, and neither is reachable
//! from a `ProcessSpec`.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lgwks_deps::tokio::process::Command;

/// The successor's half of
/// [`Supervisor::spawn_process_identified`](crate::rt::supervise::Supervisor::spawn_process_identified).
///
/// A supervisor killed with SIGKILL runs no cleanup, so its children's groups
/// outlive it. The identity each leader had, stored when it was spawned, is what
/// a later process hands to [`reap_orphaned_group`]: it stops the group only
/// while the pid still names that leader, and touches nothing when the pid is
/// gone or reissued. Named here so a consumer imports the record, its parse
/// error, the reap and its answer from the module that describes the process.
pub use lgwks_std::process::{
    OrphanReap, ProcessIdentity, ProcessIdentityError, reap_orphaned_group,
};

/// How one standard stream is connected when a process starts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum StdioPolicy {
    /// Inherit the supervisor process's stream.
    #[default]
    Inherit,
    /// Connect the stream to the platform null device.
    Null,
    /// Capture the stream for the supervisor to read back.
    ///
    /// The value is the retained-byte ceiling and is non-zero by construction,
    /// so an unbounded capture is not expressible. The **first** `limit` bytes
    /// are retained and the head of the stream is what survives truncation;
    /// the pipe keeps being drained after the ceiling is reached so a chatty
    /// child can never block on a full pipe, and every byte the child wrote is
    /// counted in [`CapturedStream::total_bytes`]. A stream with this policy is
    /// available to [`Supervisor::run_process`](crate::rt::supervise::Supervisor::run_process);
    /// [`Supervisor::spawn_process`](crate::rt::supervise::Supervisor::spawn_process)
    /// drains it too, so a captured stream never fills, but does not report it.
    Capture(NonZeroUsize),
}

/// An environment delta applied to a process at start.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EnvDelta {
    /// Set `key` to `value`.
    ///
    /// The key and value are platform-native strings.
    Set {
        /// Variable name.
        key: OsString,
        /// Variable value.
        value: OsString,
    },
    /// Remove `key` from the inherited environment.
    Remove {
        /// Variable name.
        key: OsString,
    },
    /// Start the child from an empty environment.
    ///
    /// Every variable the supervisor would otherwise pass down is dropped, and so
    /// is every delta recorded before this one; deltas recorded after it apply
    /// to the empty set. The child therefore sees exactly the assignments made
    /// after the last `Clear`, which is the allowlist a caller running an
    /// untrusted command needs: nothing of the supervisor's environment reaches
    /// the child unless the caller names it again.
    Clear,
}

/// How a supervised child is confined past its process group, declared as data.
///
/// A `ProcessSpec` is data, so this is data too: a profile, not a callback.
/// The supervisor applies it between the fork and the exec, so every
/// descendant inherits it — a confinement that only wrapped the leader's exit
/// code would be a report, not a boundary.
///
/// The Linux half — Landlock subtree-deny rules — is not here: it needs
/// syscall bindings the estate has not admitted, and no new edge is added
/// without the Director, so a subtree-deny variant would be a promise with no
/// enforcement behind it. What ships is the macOS half, enforced through the
/// platform's own `sandbox-exec`; the in-process `sandbox_init` that would
/// avoid its per-spawn cost needs an FFI leaf the workspace gate refuses, and
/// is recorded as pending where the cost is measured.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Confinement {
    /// No confinement beyond the supervisor's group and cleanup.
    #[default]
    None,
    /// A macOS sandbox profile confining the child and its descendants.
    SandboxProfile(SandboxProfile),
}

/// A macOS sandbox profile, applied to a supervised child before it execs.
///
/// The source is one `sandbox-exec -p` profile — `(version 1)(allow default)
/// (deny file-read* (subpath "/coord"))` — so the child and everything it
/// forks are confined by the kernel, while the supervisor itself is not. The
/// profile travels as a string because the supervisor applies it, never
/// compiles it: an invalid profile refuses the spawn rather than running the
/// child unconfined.
///
/// A denied subpath must name the kernel's resolved path: the sandbox matches
/// what the filesystem resolves, so a subpath spelled through a symlink
/// (macOS's symlinked `$TMPDIR` included) names a directory nothing writes
/// through. Canonicalize denied directories before building the source.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct SandboxProfile {
    /// The profile source handed to `sandbox-exec -p`.
    source: String,
}

impl SandboxProfile {
    /// The longest profile source [`Self::validate`] accepts, in bytes.
    ///
    /// A profile is data the supervisor passes to one spawn, so it is bounded
    /// like every other spawn input rather than read without limit.
    pub const MAX_SOURCE_BYTES: usize = 65_536;

    /// Carry `source` as the child's sandbox profile.
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
        }
    }

    /// The profile source handed to `sandbox-exec -p`.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Refuse a profile that could not confine anything.
    ///
    /// Empty, oversized, or non-printable sources are refused as
    /// [`std::io::ErrorKind::InvalidInput`]: the supervisor applies profiles,
    /// never repairs them, and a spawn refused here starts nothing.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::InvalidInput`] for an empty source, one past
    /// [`Self::MAX_SOURCE_BYTES`], or one carrying a byte outside printable
    /// ASCII (profiles are ASCII text; anything else is damage or an attack).
    pub fn validate(&self) -> io::Result<()> {
        if self.source.is_empty() || self.source.len() > Self::MAX_SOURCE_BYTES {
            lgwks_std::trace::warn!(
                len = self.source.len(),
                max = Self::MAX_SOURCE_BYTES,
                "SandboxProfile::validate refused an empty or oversized profile source"
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a sandbox profile is 1..={} printable bytes, got {}",
                    Self::MAX_SOURCE_BYTES,
                    self.source.len()
                ),
            ));
        }
        if let Some(at) = self
            .source
            .bytes()
            .position(|byte| !byte.is_ascii_graphic() && !byte.is_ascii_whitespace())
        {
            lgwks_std::trace::warn!(
                offset = at,
                "SandboxProfile::validate refused a profile source holding a non-text byte"
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a sandbox profile holds a non-text byte at offset {at}"),
            ));
        }
        Ok(())
    }
}

/// Pure, inspectable data describing one supervised process.
///
/// A `ProcessSpec` cannot spawn, wait, inspect status, kill, or dereference an
/// engine handle. It is deliberately `Clone` so callers can retain an audit
/// description while the supervisor owns the execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSpec {
    /// Program path or name.
    program: OsString,
    /// Ordered argument list.
    args: Vec<OsString>,
    /// Ordered environment changes.
    env: Vec<EnvDelta>,
    /// Optional working directory.
    cwd: Option<PathBuf>,
    /// Standard input policy.
    stdin: StdioPolicy,
    /// Standard output policy.
    stdout: StdioPolicy,
    /// Standard error policy.
    stderr: StdioPolicy,
    /// A file standard output is written to, replacing `stdout`'s policy.
    stdout_file: Option<PathBuf>,
    /// A file standard error is written to, replacing `stderr`'s policy.
    stderr_file: Option<PathBuf>,
    /// Optional maximum runtime.
    deadline: Option<Duration>,
    /// How the child is confined past its process group.
    confinement: Confinement,
}

impl ProcessSpec {
    /// Describe a program without starting it.
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            stdin: StdioPolicy::default(),
            stdout: StdioPolicy::default(),
            stderr: StdioPolicy::default(),
            stdout_file: None,
            stderr_file: None,
            deadline: None,
            confinement: Confinement::None,
        }
    }

    /// Add one argument.
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    /// Add an environment assignment.
    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.env.push(EnvDelta::Set {
            key: key.as_ref().to_os_string(),
            value: value.as_ref().to_os_string(),
        });
        self
    }

    /// Remove one inherited environment variable.
    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.env.push(EnvDelta::Remove {
            key: key.as_ref().to_os_string(),
        });
        self
    }

    /// Start the child from an empty environment, dropping every inherited
    /// variable and every assignment recorded before this call.
    ///
    /// Assignments made after it are the child's whole environment, so a
    /// supervisor's credentials, tokens and paths reach an untrusted command
    /// only when the caller names them again. A program named without a
    /// directory is looked up in the `PATH` the child receives, so a cleared
    /// environment that sets no `PATH` searches the platform's default path
    /// rather than the supervisor's; name the program by its absolute path, or
    /// set `PATH` after the clear, when that difference matters.
    pub fn env_clear(&mut self) -> &mut Self {
        self.env.push(EnvDelta::Clear);
        self
    }

    /// Set the process working directory.
    pub fn current_dir(&mut self, cwd: impl AsRef<Path>) -> &mut Self {
        self.cwd = Some(cwd.as_ref().to_path_buf());
        self
    }

    /// Set the stdin policy.
    pub fn stdin(&mut self, policy: StdioPolicy) -> &mut Self {
        self.stdin = policy;
        self
    }

    /// Set the stdout policy.
    pub fn stdout(&mut self, policy: StdioPolicy) -> &mut Self {
        self.stdout = policy;
        self.stdout_file = None;
        self
    }

    /// Set the stderr policy.
    pub fn stderr(&mut self, policy: StdioPolicy) -> &mut Self {
        self.stderr = policy;
        self.stderr_file = None;
        self
    }

    /// Capture stdout, retaining at most `limit` bytes.
    ///
    /// Shorthand for `stdout(StdioPolicy::Capture(limit))`; the value is
    /// non-zero by its type, and every byte the child writes is counted even
    /// past the retained ceiling.
    pub fn capture_stdout(&mut self, limit: NonZeroUsize) -> &mut Self {
        self.stdout = StdioPolicy::Capture(limit);
        self.stdout_file = None;
        self
    }

    /// Capture stderr, retaining at most `limit` bytes.
    ///
    /// The stderr counterpart of [`ProcessSpec::capture_stdout`].
    pub fn capture_stderr(&mut self, limit: NonZeroUsize) -> &mut Self {
        self.stderr = StdioPolicy::Capture(limit);
        self.stderr_file = None;
        self
    }

    /// Write standard output to the file at `path`, creating it if it is absent
    /// and truncating it if it is not.
    ///
    /// For a tool that writes its result to stdout and has no output-path flag.
    /// The file is opened when the child starts, so a path that cannot be opened
    /// refuses the start as an [`io::Error`] and no child runs. The stream is not
    /// captured: [`ProcessRun::stdout`] is empty. Choosing a stdout policy or a
    /// capture afterwards replaces this.
    pub fn stdout_to_file(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.stdout = StdioPolicy::Null;
        self.stdout_file = Some(path.as_ref().to_path_buf());
        self
    }

    /// Write standard error to the file at `path`, as
    /// [`stdout_to_file`](Self::stdout_to_file) does for standard output.
    pub fn stderr_to_file(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.stderr = StdioPolicy::Null;
        self.stderr_file = Some(path.as_ref().to_path_buf());
        self
    }

    /// Set the maximum runtime before the supervisor stops the process group.
    pub fn deadline(&mut self, deadline: Duration) -> &mut Self {
        self.deadline = Some(deadline);
        self
    }

    /// Confine the child past its process group.
    ///
    /// A [`Confinement::SandboxProfile`] wraps the spawn in the platform's
    /// sandbox on macOS and refuses it anywhere else, so a profile set by
    /// mistake on Linux or Windows starts nothing rather than running the
    /// child unconfined. Confinement applies between the fork and the exec,
    /// so every descendant inherits it.
    pub fn confinement(&mut self, confinement: Confinement) -> &mut Self {
        self.confinement = confinement;
        self
    }

    /// The program to execute, for inspection only.
    #[must_use]
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    /// The ordered arguments, for inspection only.
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// The ordered environment deltas, for inspection only.
    #[must_use]
    pub fn env_deltas(&self) -> &[EnvDelta] {
        &self.env
    }

    /// The optional working directory, for inspection only.
    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// How the child's standard input was wired at spawn.
    ///
    /// The policy is what was *requested*; it does not report what the child
    /// then read, which nothing here can know. `Capture` carries the retained
    /// byte ceiling, so this also answers how much input this child could have
    /// had captured rather than piped.
    #[must_use]
    pub const fn stdin_policy(&self) -> StdioPolicy {
        self.stdin
    }

    /// How the child's standard output was wired at spawn.
    ///
    /// `Capture` means [`Self::stdout`] holds what the child wrote, up to the
    /// ceiling this value names; `Inherit` and `Null` mean the supervisor never
    /// saw it and [`Self::stdout`] is empty rather than absent.
    #[must_use]
    pub const fn stdout_policy(&self) -> StdioPolicy {
        self.stdout
    }

    /// How the child's standard error was wired at spawn.
    ///
    /// The same three choices as [`Self::stdout_policy`], kept separate because
    /// a supervisor commonly captures diagnostics while letting ordinary output
    /// through, and a caller asking "did I get its stderr" reads this.
    #[must_use]
    pub const fn stderr_policy(&self) -> StdioPolicy {
        self.stderr
    }

    /// The optional runtime deadline, for inspection only.
    #[must_use]
    pub const fn deadline_duration(&self) -> Option<Duration> {
        self.deadline
    }

    /// How the child is confined past its process group, for inspection only.
    ///
    /// [`Confinement::None`] unless [`Self::confinement`] named a profile: a
    /// spec that never asked for confinement must not read as one that did.
    #[must_use]
    pub const fn confinement_policy(&self) -> &Confinement {
        &self.confinement
    }

    /// The retained-byte ceiling for stdout, or `None` when it is not captured.
    #[must_use]
    pub(crate) const fn stdout_capture(&self) -> Option<NonZeroUsize> {
        match self.stdout {
            StdioPolicy::Capture(limit) => Some(limit),
            StdioPolicy::Inherit | StdioPolicy::Null => None,
        }
    }

    /// The retained-byte ceiling for stderr, or `None` when it is not captured.
    #[must_use]
    pub(crate) const fn stderr_capture(&self) -> Option<NonZeroUsize> {
        match self.stderr {
            StdioPolicy::Capture(limit) => Some(limit),
            StdioPolicy::Inherit | StdioPolicy::Null => None,
        }
    }

    /// Build the engine command's program: the spec's own, or the sandbox wrapper.
    ///
    /// A confined spawn replaces the program with the platform's sandbox and
    /// carries the target as its operand (see [`Self::configure`]), so the
    /// wrapper confines itself before it execs the target and the profile
    /// holds the child and every descendant.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::InvalidInput`] for a profile that could not
    /// confine anything, [`std::io::ErrorKind::Unsupported`] for a profile on
    /// a target without the platform's sandbox. Both refuse before the fork,
    /// so neither can run a child unconfined.
    pub(crate) fn command(&self) -> io::Result<Command> {
        match self.confinement {
            Confinement::None => Ok(Command::new(self.resolved_program())),
            Confinement::SandboxProfile(ref profile) => {
                profile.validate()?;
                #[cfg(target_os = "macos")]
                {
                    Ok(sandbox_command(profile))
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "lgwks_bot: a sandbox profile confines a child on macOS only",
                    ))
                }
            }
        }
    }

    /// The program the engine command spawns: resolved against the child's
    /// `PATH` when it names no directory.
    ///
    /// `std` spawns through `posix_spawn` only when the program already holds
    /// a `/`; a bare name with a pinned `PATH` (see [`Self::env`]) falls back
    /// to `fork` + `execvp`, which copies the supervisor's page tables and
    /// runs every allocator's at-fork handlers on each spawn. Resolving here
    /// takes the fast path for the case production callers hit.
    ///
    /// The rule is that resolution may only ever name **the file `execvp`
    /// would have run**; anything it cannot decide stays bare and takes the
    /// fork path unchanged. The lookup folds the spec's own deltas over the
    /// inherited `PATH`, in order, so the search sees what the child will see
    /// (INV-BOT-159): after a `Clear` that sets no `PATH`, or a `Remove`, the
    /// path is unknown and the program stays bare. Each entry is judged the
    /// way `execve` judges it — symlinks followed — and an entry `execvp`
    /// would pass over (absent, a directory, no execute bit at all, a search
    /// it may not make) is passed over here. An entry it would *try* ends the
    /// walk: a regular file with every execute bit and a directly executable
    /// head (a `#!` line naming an existing executable interpreter, or this
    /// platform's native binary magic) resolves; anything else — partial
    /// execute bits, a head that could not be read, a shell-fallback script —
    /// stays bare, because whether `execvp` runs it or moves on is the
    /// kernel's call for this caller, not a guess this walk may make. A
    /// working directory that is not absolute stays bare too: `Command` would
    /// read a relative program path against the child's directory a second
    /// time.
    ///
    /// **Not claimed:** the walk happens before the fork rather than after, so
    /// a `PATH` entry replaced in between resolves to the earlier file; and a
    /// native binary whose loader is missing fails with `ENOENT` where
    /// `execvp` would have tried a later entry of the same name.
    pub(crate) fn resolved_program(&self) -> PathBuf {
        #[cfg(unix)]
        if let Some(resolved) = resolve_on_path(&self.program, &self.env, self.cwd.as_deref()) {
            return resolved;
        }
        PathBuf::from(&self.program)
    }

    /// Configure the private engine command owned by the supervisor.
    ///
    /// # Errors
    ///
    /// The [`io::Error`] from opening a file named by
    /// [`stdout_to_file`](Self::stdout_to_file) or
    /// [`stderr_to_file`](Self::stderr_to_file).
    pub(crate) fn configure(&self, command: &mut Command) -> io::Result<()> {
        match self.confinement {
            Confinement::None => {
                command.args(&self.args);
            }
            Confinement::SandboxProfile(_) => {
                // The wrapper's first operand is the target: `sandbox-exec -p
                // <profile> <program> <args...>`. The program travels as an
                // argument here because [`Self::command`] already replaced it
                // with the wrapper.
                command.arg(&self.program);
                command.args(&self.args);
            }
        }
        for delta in &self.env {
            match *delta {
                EnvDelta::Set { ref key, ref value } => {
                    command.env(key, value);
                }
                EnvDelta::Remove { ref key } => {
                    command.env_remove(key);
                }
                EnvDelta::Clear => {
                    command.env_clear();
                }
            }
        }
        if let Some(cwd) = self.cwd.as_ref() {
            command.current_dir(cwd);
        }
        command.stdin(self.stdin.into_stdio());
        command.stdout(match self.stdout_file.as_deref() {
            Some(path) => file_stdio(path)?,
            None => self.stdout.into_stdio(),
        });
        command.stderr(match self.stderr_file.as_deref() {
            Some(path) => file_stdio(path)?,
            None => self.stderr.into_stdio(),
        });
        Ok(())
    }
}

/// How many `PATH` entries [`resolve_on_path`] walks before giving up.
///
/// A `PATH` is caller data of unbounded length; the walk is a spawn input and
/// is bounded like every other one. Past the cap the program stays bare and
/// the spawn takes the fork path, exactly as before this resolution existed.
#[cfg(unix)]
const MAX_PATH_ENTRIES: usize = 128;

/// How many bytes of a candidate's head [`probe_entry`] reads: enough for a
/// `#!` line naming an interpreter by an ordinary absolute path.
#[cfg(unix)]
const MAX_HEAD_BYTES: usize = 256;

/// Resolve `program` against the child's `PATH` the way `execvp` would, or
/// `None` to spawn it bare.
///
/// `None` means the name holds a `/`, the path is unknown, the base directory
/// is not absolute, nothing executable was found, or an entry was found whose
/// fate only the kernel can decide. Every `None` takes the pre-existing fork
/// path, so resolution can only add the fast path, never change what runs.
#[cfg(unix)]
fn resolve_on_path(program: &OsStr, deltas: &[EnvDelta], cwd: Option<&Path>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    if program.is_empty() || program.as_bytes().contains(&b'/') {
        return None;
    }
    // A spec that leaves `PATH` alone already spawns through `posix_spawnp`,
    // which searches the inherited path itself: walking it here would only add
    // a stat, an open and a read to every ordinary spawn. `std` falls back to
    // the fork exactly when the path was changed, so that is when this walks.
    if !deltas.iter().any(EnvDelta::changes_path) {
        return None;
    }
    let path = effective_path(deltas)?;
    let base = match cwd {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir().ok()?,
    };
    if !base.is_absolute() {
        return None;
    }
    for (index, entry) in path.as_bytes().split(|byte| *byte == b':').enumerate() {
        if index >= MAX_PATH_ENTRIES {
            return None;
        }
        // An empty entry is the child's working directory; a relative one is
        // read against it; an absolute one replaces it.
        let candidate = base.join(OsStr::from_bytes(entry)).join(program);
        match probe_entry(&candidate) {
            Entry::PassedOver => {}
            Entry::Spawnable => return Some(candidate),
            Entry::Undecided => return None,
        }
    }
    None
}

#[cfg(unix)]
impl EnvDelta {
    /// Whether this delta changes the child's `PATH`, the condition under which
    /// `std` gives up `posix_spawnp` for a bare program.
    fn changes_path(&self) -> bool {
        match *self {
            Self::Set { ref key, .. } | Self::Remove { ref key } => key.as_os_str() == "PATH",
            Self::Clear => true,
        }
    }
}

/// The `PATH` the child will see: the inherited one folded through `deltas`.
///
/// `Clear` drops the inherited path and everything before it; a later `Set`
/// rebuilds it. `None` is "unknown": cleared-and-unset, removed, or never
/// inherited. Callers leave the program bare then, for the platform default.
#[cfg(unix)]
fn effective_path(deltas: &[EnvDelta]) -> Option<OsString> {
    let mut path = std::env::var_os("PATH");
    for delta in deltas {
        match *delta {
            EnvDelta::Set { ref key, ref value } if key.as_os_str() == "PATH" => {
                path = Some(value.as_os_str().to_os_string());
            }
            EnvDelta::Remove { ref key } if key.as_os_str() == "PATH" => {
                path = None;
            }
            EnvDelta::Clear => {
                path = None;
            }
            EnvDelta::Set { .. } | EnvDelta::Remove { .. } => {}
        }
    }
    path
}

/// What `execvp` would do with one `PATH` candidate.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    /// `execvp` moves on to the next entry: absent, a directory, no execute
    /// bit, or a search it may not make.
    PassedOver,
    /// The kernel runs it directly, for any caller: the walk resolves here.
    Spawnable,
    /// `execvp` would try it, and only the kernel knows what happens: the
    /// walk stops and the program stays bare.
    Undecided,
}

/// Judge one candidate the way `execve` would, symlinks followed.
#[cfg(unix)]
fn probe_entry(candidate: &Path) -> Entry {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = match std::fs::metadata(candidate) {
        Ok(metadata) => metadata,
        // The three errors `execvp` searches past; any other is the kernel's.
        Err(error) => {
            return match error.kind() {
                io::ErrorKind::NotFound
                | io::ErrorKind::NotADirectory
                | io::ErrorKind::PermissionDenied => Entry::PassedOver,
                _ => {
                    lgwks_std::trace::debug!(
                        path = ?candidate,
                        error = %error,
                        "resolve_on_path left the program bare at a candidate it could not stat"
                    );
                    Entry::Undecided
                }
            };
        }
    };
    if metadata.is_dir() {
        return Entry::PassedOver;
    }
    match metadata.permissions().mode() & 0o111 {
        0 => return Entry::PassedOver,
        0o111 => {}
        // Executable by someone: whether by this caller is the kernel's call.
        _ => return Entry::Undecided,
    }
    if !metadata.is_file() {
        return Entry::Undecided;
    }
    let mut head = Vec::with_capacity(MAX_HEAD_BYTES);
    if let Err(error) = read_head(candidate, &mut head) {
        // An execute-only file is still executable: an unreadable head is the
        // kernel's to judge, never a reason to try the next entry.
        lgwks_std::trace::debug!(
            path = ?candidate,
            error = %error,
            "resolve_on_path left the program bare at a head it could not read"
        );
        return Entry::Undecided;
    }
    if is_direct_head(&head) {
        Entry::Spawnable
    } else {
        Entry::Undecided
    }
}

/// Read up to [`MAX_HEAD_BYTES`] of `candidate` into `head`.
#[cfg(unix)]
fn read_head(candidate: &Path, head: &mut Vec<u8>) -> io::Result<()> {
    use std::io::Read as _;
    let file = std::fs::File::open(candidate)?;
    let limit = u64::try_from(MAX_HEAD_BYTES).map_err(io::Error::other)?;
    file.take(limit).read_to_end(head)?;
    Ok(())
}

/// Whether the kernel executes a file with this head without a shell: a
/// `#!` line whose interpreter is an existing regular file with every execute
/// bit, or this platform's native binary magic.
#[cfg(unix)]
fn is_direct_head(head: &[u8]) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(line) = head.strip_prefix(b"#!") {
        // A `#!` line longer than the head names no ordinary interpreter.
        let Some(end) = line.iter().position(|byte| *byte == b'\n') else {
            return false;
        };
        let Some((line, _)) = line.split_at_checked(end) else {
            return false;
        };
        let Some(interpreter) = line
            .split(|byte| *byte == b' ' || *byte == b'\t')
            .find(|word| !word.is_empty())
        else {
            return false;
        };
        let interpreter = Path::new(OsStr::from_bytes(interpreter));
        if !interpreter.is_absolute() {
            return false;
        }
        // An interpreter that cannot be stat'd cannot be exec'd either, so the
        // refusal is the answer "not runnable", not a default standing in for one.
        return match std::fs::metadata(interpreter) {
            Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 == 0o111,
            Err(_) => false,
        };
    }
    is_native_binary(head)
}

/// An ELF image: the native format of every non-Apple Unix this crate builds
/// for. A Mach-O there is `ENOEXEC`, which `execvp` hands to the shell.
#[cfg(all(unix, not(target_vendor = "apple")))]
fn is_native_binary(head: &[u8]) -> bool {
    head.starts_with(b"\x7fELF")
}

/// A Mach-O image, thin or universal. A universal header shares its magic with
/// a Java class file, whose next word is a version (45 or more) where a
/// universal binary's is its architecture count, so only a small count is a
/// binary. An ELF image here is `ENOEXEC`, which `execvp` hands to the shell.
#[cfg(target_vendor = "apple")]
fn is_native_binary(head: &[u8]) -> bool {
    /// More architectures than any universal binary carries, and fewer than
    /// the first Java class-file version.
    const MAX_FAT_ARCHS: u32 = 20;
    let Some((magic, rest)) = head.split_first_chunk::<4>() else {
        return false;
    };
    match *magic {
        [0xcf | 0xce, 0xfa, 0xed, 0xfe] | [0xfe, 0xed, 0xfa, 0xce | 0xcf] => true,
        [0xca, 0xfe, 0xba, 0xbe | 0xbf] => rest
            .first_chunk::<4>()
            .map(|count| u32::from_be_bytes(*count))
            .is_some_and(|count| count > 0 && count < MAX_FAT_ARCHS),
        _ => false,
    }
}

/// The `sandbox-exec` wrapper for a confined spawn (macOS).
///
/// `sandbox-exec -p <profile>`: the wrapper confines itself before it execs
/// the target, so the profile holds the child and everything it forks. The
/// profile was validated by the caller; this names the wrapper. The per-spawn
/// cost of the extra fork and exec is measured, not assumed — see the
/// confinement evidence in `docs/production-readiness.md` §4.7 — and the
/// in-process `sandbox_init` that would avoid it is pending on an FFI leaf the
/// workspace gate refuses.
#[cfg(target_os = "macos")]
fn sandbox_command(profile: &SandboxProfile) -> Command {
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.arg("-p").arg(profile.source());
    command
}

/// Open `path` for a child's output: created if absent, truncated if present.
fn file_stdio(path: &Path) -> io::Result<std::process::Stdio> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    Ok(std::process::Stdio::from(file))
}

impl StdioPolicy {
    /// Convert the data policy into the private engine's standard stream value.
    fn into_stdio(self) -> std::process::Stdio {
        match self {
            Self::Inherit => std::process::Stdio::inherit(),
            Self::Null => std::process::Stdio::null(),
            // The supervisor reads the pipe back while the child runs, so a
            // captured stream never fills and blocks the child.
            Self::Capture(_) => std::process::Stdio::piped(),
        }
    }
}

/// Why [`Supervisor::run_process`](crate::rt::supervise::Supervisor::run_process)
/// could not produce a [`ProcessRun`].
///
/// The vocabulary draws the one distinction a caller's next move turns on:
/// whether the program ever ran. [`ProcessRunError::Refused`] and
/// [`ProcessRunError::NotStarted`] establish that nothing ran, so a retry is
/// safe; [`ProcessRunError::AfterStart`] establishes that the child was
/// started and its outcome is unknown, so a retry is a possible duplicate.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProcessRunError {
    /// The supervisor refused to start anything: it was cancelled before the
    /// fork, so no instruction of the program ran.
    Refused,
    /// The platform refused to start the program at all — it does not exist,
    /// is not executable, or the fork was refused. Nothing ran.
    NotStarted {
        /// The platform's refusal.
        source: io::Error,
    },
    /// The process started and was then stopped by this supervisor before it
    /// exited on its own, or its terminal status could not be read. It ran, so
    /// the effect is indeterminate.
    AfterStart {
        /// Why the run could not be settled.
        source: io::Error,
    },
}

impl fmt::Display for ProcessRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Refused => formatter.write_str("the supervisor refused to start a process"),
            Self::NotStarted { ref source } => {
                write!(formatter, "the program did not start: {source}")
            }
            Self::AfterStart { ref source } => {
                write!(
                    formatter,
                    "the process started but did not settle: {source}"
                )
            }
        }
    }
}

impl std::error::Error for ProcessRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Refused => None,
            Self::NotStarted { ref source } | Self::AfterStart { ref source } => Some(source),
        }
    }
}

#[cfg(all(test, unix))]
mod resolution_tests;

// What a run hands back and how its framed output is read, each in its own
// file; re-exported so every path stays `rt::process::<Name>`.
mod frames;
mod run;

pub use frames::{DEFAULT_FRAME_CEILING, FrameRead, FrameReadError, Frames, read_frames};
pub use run::{CapturedStream, ProcessRun};
