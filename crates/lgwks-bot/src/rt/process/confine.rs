//! How a supervised child is confined past its process group (#263, #337).
//!
//! Every refusal here happens before the fork, so a confinement the platform
//! cannot honour starts nothing rather than running the child unconfined.

use std::io;
use std::path::{Path, PathBuf};

use lgwks_deps::tokio::process::Command;

/// How a supervised child is confined past its process group, declared as data.
///
/// A `ProcessSpec` is data, so this is data too: a profile, not a callback.
/// The supervisor applies it between the fork and the exec, so every
/// descendant inherits it — a confinement that only wrapped the leader's exit
/// code would be a report, not a boundary.
///
/// Both forms are exec wrappers the platform ships — `sandbox-exec` on macOS,
/// util-linux `setpriv` on Linux — so neither needs `unsafe` (forbidden
/// workspace-wide) nor a new edge. The in-process forms that would avoid the
/// wrapper's extra exec (`sandbox_init`, the raw Landlock syscalls) need an
/// FFI leaf the workspace gate refuses; that cost is measured in
/// `docs/production-readiness.md` §4.7.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Confinement {
    /// No confinement beyond the supervisor's group and cleanup.
    #[default]
    None,
    /// A macOS sandbox profile confining the child and its descendants.
    SandboxProfile(SandboxProfile),
    /// A Linux Landlock filesystem allowlist confining the child and its
    /// descendants.
    Landlock(LandlockRules),
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

/// What a Landlock rule lets a confined child do beneath one path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PathAccess {
    /// Execute files, read files and list directories.
    Read,
    /// Everything [`Self::Read`] allows, plus creating, writing, truncating,
    /// renaming and removing files and directories.
    ReadWrite,
}

impl PathAccess {
    /// The `setpriv` access list this grants, in util-linux's vocabulary.
    #[cfg(target_os = "linux")]
    const fn setpriv_list(self) -> &'static str {
        match self {
            Self::Read => "execute,read-file,read-dir",
            Self::ReadWrite => {
                "execute,write-file,read-file,read-dir,remove-dir,remove-file,\
                 make-char,make-dir,make-reg,make-sock,make-fifo,make-block,make-sym,refer,truncate"
            }
        }
    }
}

/// A Landlock filesystem allowlist for a supervised child (Linux).
///
/// Landlock is deny-by-default: once applied, the child and every descendant
/// may touch the filesystem only beneath the paths named here, with the access
/// named for each. Anything unnamed — the coordinator's keys, its database,
/// another tenant's tree — is refused by the kernel with `EACCES`. The child
/// needs its own program, its loader and its libraries, so a typical rule set
/// grants [`PathAccess::Read`] beneath `/usr`, `/lib` and `/etc` and
/// [`PathAccess::ReadWrite`] beneath its working directory.
///
/// A rule names an existing path, matched as the kernel resolves it when the
/// rule is applied; a rule naming a missing path refuses the spawn rather than
/// silently granting nothing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct LandlockRules {
    /// `(path, access)` grants, in the order they were named.
    rules: Vec<(PathBuf, PathAccess)>,
}

impl LandlockRules {
    /// The most rules one confinement carries.
    ///
    /// Each rule is one argument pair to the wrapper and one kernel rule, so
    /// the list is bounded like every other spawn input.
    pub const MAX_RULES: usize = 64;

    /// The longest path a rule names, in bytes (Linux's `PATH_MAX`).
    pub const MAX_PATH_BYTES: usize = 4_096;

    /// An empty allowlist; [`Self::validate`] refuses it until a rule is added.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant `access` beneath `path`.
    pub fn allow(&mut self, path: impl AsRef<Path>, access: PathAccess) -> &mut Self {
        self.rules.push((path.as_ref().to_path_buf(), access));
        self
    }

    /// The grants, in the order they were named, for inspection only.
    pub fn rules(&self) -> impl ExactSizeIterator<Item = (&Path, PathAccess)> {
        self.rules
            .iter()
            .map(|&(ref path, access)| (path.as_path(), access))
    }

    /// Refuse a rule list that could not confine anything or that the wrapper
    /// could misread.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::InvalidInput`] for an empty list, one past
    /// [`Self::MAX_RULES`], or a path that is relative, longer than
    /// [`Self::MAX_PATH_BYTES`], not UTF-8, or holding a NUL or a newline.
    pub fn validate(&self) -> io::Result<()> {
        let Some(message) = self.refusal() else {
            return Ok(());
        };
        lgwks_std::trace::warn!(%message, "LandlockRules::validate refused an allowlist before the fork");
        Err(io::Error::new(io::ErrorKind::InvalidInput, message))
    }

    /// Why this list cannot be applied, or `None` when it can.
    fn refusal(&self) -> Option<String> {
        if self.rules.is_empty() || self.rules.len() > Self::MAX_RULES {
            return Some(format!(
                "a Landlock allowlist holds 1..={} rules, got {}",
                Self::MAX_RULES,
                self.rules.len()
            ));
        }
        self.rules().find_map(|(path, _)| {
            // A path that is not UTF-8 cannot be handed to the wrapper intact.
            let fits = match path.to_str() {
                Some(text) => text.len() <= Self::MAX_PATH_BYTES && !text.contains(['\0', '\n']),
                None => false,
            };
            (!fits || !path.is_absolute()).then(|| {
                format!(
                    "a Landlock path is UTF-8, absolute, at most {} bytes, with no NUL or newline: {path:?}",
                    Self::MAX_PATH_BYTES
                )
            })
        })
    }
}

/// The engine command for `program` under `confinement`: the program itself,
/// or the platform's wrapper with the program as its operand.
///
/// # Errors
///
/// [`std::io::ErrorKind::InvalidInput`] for a confinement that could not
/// confine anything, [`std::io::ErrorKind::NotFound`] for a Landlock rule
/// naming a missing path, and [`std::io::ErrorKind::Unsupported`] where the
/// platform has no such wrapper. Each refuses before the fork, so none can
/// run a child unconfined.
pub(super) fn command(confinement: &Confinement, program: &Path) -> io::Result<Command> {
    match *confinement {
        Confinement::None => Ok(Command::new(program)),
        Confinement::SandboxProfile(ref profile) => {
            profile.validate()?;
            sandbox(profile)
        }
        Confinement::Landlock(ref rules) => {
            rules.validate()?;
            landlock(rules)
        }
    }
}

/// Whether `confinement` replaces the program with a wrapper, so the program
/// travels as the wrapper's first operand.
pub(super) const fn wraps(confinement: &Confinement) -> bool {
    !matches!(*confinement, Confinement::None)
}

/// `sandbox-exec -p <profile>` (macOS): the wrapper confines itself before it
/// execs the target, so the profile holds the child and everything it forks.
#[cfg(target_os = "macos")]
fn sandbox(profile: &SandboxProfile) -> io::Result<Command> {
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.arg("-p").arg(profile.source());
    Ok(command)
}

/// Every other target has no Seatbelt.
#[cfg(not(target_os = "macos"))]
fn sandbox(_profile: &SandboxProfile) -> io::Result<Command> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "lgwks_bot: a sandbox profile confines a child on macOS only",
    ))
}

/// The util-linux wrapper that applies Landlock rules and then execs.
#[cfg(target_os = "linux")]
const SETPRIV: &str = "/usr/bin/setpriv";

/// `setpriv --landlock-access fs --landlock-rule path-beneath:<access>:<path>
/// ... --` (Linux): the wrapper restricts itself and then execs the target,
/// so the ruleset holds the child and every descendant.
#[cfg(target_os = "linux")]
fn landlock(rules: &LandlockRules) -> io::Result<Command> {
    setpriv_supports_landlock()?;
    let mut command = Command::new(SETPRIV);
    command.arg("--landlock-access").arg("fs");
    for (path, access) in rules.rules() {
        if !path.exists() {
            lgwks_std::trace::warn!(?path, "a Landlock rule names a missing path");
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("a Landlock rule names a missing path: {}", path.display()),
            ));
        }
        let mut rule = std::ffi::OsString::from("path-beneath:");
        rule.push(access.setpriv_list());
        rule.push(":");
        rule.push(path.as_os_str());
        command.arg("--landlock-rule").arg(rule);
    }
    command.arg("--");
    Ok(command)
}

/// Every other target has no Landlock.
#[cfg(not(target_os = "linux"))]
fn landlock(_rules: &LandlockRules) -> io::Result<Command> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "lgwks_bot: a Landlock allowlist confines a child on Linux only",
    ))
}

/// Whether this host's `setpriv` speaks Landlock, asked once per process.
///
/// util-linux added `--landlock-access` in 2.40; an older `setpriv` would exit
/// non-zero with the target never run, which is fail-closed but reads as the
/// child's own failure. Asking first turns that into an `Unsupported` refusal
/// before the fork. The probe is one `setpriv --help`, cached for the life of
/// the process.
#[cfg(target_os = "linux")]
fn setpriv_supports_landlock() -> io::Result<()> {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let supported = *SUPPORTED.get_or_init(|| {
        match std::process::Command::new(SETPRIV)
            .arg("--help")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        {
            Ok(output) => output
                .stdout
                .windows(b"--landlock-access".len())
                .any(|window| window == b"--landlock-access"),
            // No runnable `setpriv` is no Landlock wrapper: the cause is
            // logged, and every Landlock spawn is refused below.
            Err(error) => {
                lgwks_std::trace::warn!(%error, "setpriv --help could not run");
                false
            }
        }
    });
    if supported {
        Ok(())
    } else {
        lgwks_std::trace::warn!("setpriv on this host has no Landlock support");
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "lgwks_bot: Landlock confinement needs util-linux setpriv 2.40 or later at /usr/bin/setpriv",
        ))
    }
}
