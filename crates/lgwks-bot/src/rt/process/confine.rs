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
/// All forms are exec wrappers the platform ships — `sandbox-exec` on macOS,
/// util-linux `setpriv` or a mount-namespace wrapper on Linux — so none needs
/// `unsafe` (forbidden workspace-wide) nor a new edge. The in-process forms
/// that would avoid the wrapper's extra exec (`sandbox_init`, the raw
/// Landlock syscalls) need an FFI leaf the workspace gate refuses; that cost
/// is measured in `docs/production-readiness.md` §4.7.
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
    /// A deny-shaped confinement: everything is allowed except the named
    /// subtrees, which the child and its descendants cannot read.
    DenyBeneath(DenyBeneath),
}

impl Confinement {
    /// Confine the child everywhere except beneath `paths`.
    ///
    /// The estate computes the denial as the platform allows: a sandbox
    /// profile denying reads beneath each path on macOS, an empty `tmpfs`
    /// overmounted on each path in a mount namespace on Linux. An allowlist
    /// ([`LandlockRules`]) cannot say "everything but the coordinator's
    /// state", because it would have to name every sibling of every ancestor
    /// of that state; this names only the state.
    #[must_use]
    pub fn deny_beneath(paths: impl IntoIterator<Item = impl AsRef<Path>>) -> Self {
        let mut denied = DenyBeneath::new();
        for path in paths {
            denied.deny(path);
        }
        Self::DenyBeneath(denied)
    }
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
/// through. Canonicalize denied directories before building the source, which
/// is what [`Self::deny_read_beneath`] does for its caller.
///
/// The profile source is UTF-8 text: SBPL string literals take UTF-8, so a
/// denied directory under a non-ASCII home confines rather than refusing the
/// spawn. Only control bytes are refused, at validation and at construction.
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
    ///
    /// Infallible for the callers that build their source from literals;
    /// [`Self::try_new`] validates eagerly for every other caller.
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
        }
    }

    /// Carry `source` as the child's sandbox profile, refusing what could not
    /// confine anything before the caller goes further.
    ///
    /// # Errors
    ///
    /// Exactly what [`Self::validate`] refuses: an empty or oversized source,
    /// or one carrying a refused control byte.
    pub fn try_new(source: impl Into<String>) -> io::Result<Self> {
        let profile = Self {
            source: source.into(),
        };
        profile.validate()?;
        Ok(profile)
    }

    /// The `(version 1)(allow default)` profile: the child keeps everything
    /// the platform allows, and each [`Self::deny_read_beneath`] call takes
    /// one subtree away.
    #[must_use]
    pub fn allow_default() -> Self {
        Self {
            source: String::from("(version 1)(allow default)"),
        }
    }

    /// Deny reads beneath `path` in this profile.
    ///
    /// The denied directory is canonicalized first, so a `$TMPDIR` spelled
    /// through a symlink names the directory the kernel resolves, and quoted
    /// for SBPL, so a `"` or `\` in the path cannot break out of the
    /// subpath literal. The refusal names the path, at construction, far
    /// from the spawn it would otherwise fail.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::InvalidInput`] for a path that cannot be
    /// resolved, is not UTF-8, or carries a control byte the profile source
    /// cannot hold, and for a rule that would push the source past
    /// [`Self::MAX_SOURCE_BYTES`].
    pub fn deny_read_beneath(&mut self, path: impl AsRef<Path>) -> io::Result<&mut Self> {
        let resolved = std::fs::canonicalize(path.as_ref()).map_err(|source| {
            io::Error::new(
                source.kind(),
                format!(
                    "a sandbox denial resolves its directory before quoting it: {}: {source}",
                    path.as_ref().display()
                ),
            )
        })?;
        let text = resolved.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a sandbox subpath is UTF-8 text: {}", resolved.display()),
            )
        })?;
        if let Some(at) = text.bytes().position(|byte| byte < 0x20 || byte == 0x7f) {
            lgwks_std::trace::warn!(
                offset = at,
                "SandboxProfile::deny_read_beneath refused a denied path holding a control byte"
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a sandbox denial holds no control byte, at offset {at}"),
            ));
        }
        let mut rule = String::from("(deny file-read* (subpath \"");
        for character in text.chars() {
            match character {
                '"' => rule.push_str("\\\""),
                '\\' => rule.push_str("\\\\"),
                _ => rule.push(character),
            }
        }
        rule.push_str("\"))");
        if self.source.len().saturating_add(rule.len()) > Self::MAX_SOURCE_BYTES {
            lgwks_std::trace::warn!(
                max = Self::MAX_SOURCE_BYTES,
                "SandboxProfile::deny_read_beneath refused a denial past the source bound"
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a sandbox profile is 1..={} printable bytes, this denial would pass it",
                    Self::MAX_SOURCE_BYTES
                ),
            ));
        }
        self.source.push_str(&rule);
        Ok(self)
    }

    /// The profile source handed to `sandbox-exec -p`.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Refuse a profile that could not confine anything.
    ///
    /// Empty, oversized, or control-byte sources are refused as
    /// [`std::io::ErrorKind::InvalidInput`]: the supervisor applies profiles,
    /// never repairs them, and a spawn refused here starts nothing.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::InvalidInput`] for an empty source, one past
    /// [`Self::MAX_SOURCE_BYTES`], or one carrying an ASCII control byte
    /// other than tab or newline. Bytes at or above `0x80` are UTF-8 text a
    /// denied non-ASCII path is spelled in, not damage.
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
                    "a sandbox profile is 1..={} bytes, got {}",
                    Self::MAX_SOURCE_BYTES,
                    self.source.len()
                ),
            ));
        }
        if let Some(at) = self
            .source
            .bytes()
            .position(|byte| byte < 0x20 && !matches!(byte, b'\t' | b'\n') || byte == 0x7f)
        {
            lgwks_std::trace::warn!(
                offset = at,
                "SandboxProfile::validate refused a profile source holding a control byte"
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a sandbox profile holds a control byte at offset {at}"),
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
///
/// An allowlist cannot contain a general build run from a sibling directory
/// of the denied state: it would have to name every sibling of every
/// ancestor of that state, plus everything any build script reads, within
/// [`Self::MAX_RULES`]. For "everything but this subtree" use
/// [`Confinement::deny_beneath`], which the estate computes as the platform
/// allows.
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

/// A deny-shaped confinement: everything is allowed except the named subtrees.
///
/// An allowlist cannot contain a general build run from a sibling directory
/// of the coordinator's state: it would have to name every sibling of every
/// ancestor of that state, plus everything any build script reads, within
/// [`LandlockRules::MAX_RULES`]. This names only the denied subtrees, and the
/// estate overmounts each with an empty `tmpfs` on Linux (through `bwrap`,
/// else a mount namespace) or denies reads beneath it in the sandbox profile
/// on macOS, so the child and every descendant lose exactly those directories.
///
/// A rule names an existing directory, matched as the kernel resolves it when
/// the confinement is applied; a rule naming a missing path or a
/// non-directory refuses the spawn rather than silently granting nothing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct DenyBeneath {
    /// Subtrees the child and its descendants cannot read, in the order named.
    paths: Vec<PathBuf>,
}

impl DenyBeneath {
    /// The most denied subtrees one confinement carries.
    ///
    /// Each denial is one overmount and one wrapper argument, so the list is
    /// bounded like every other spawn input.
    pub const MAX_PATHS: usize = 64;

    /// The longest path a denial names, in bytes (Linux's `PATH_MAX`).
    pub const MAX_PATH_BYTES: usize = 4_096;

    /// No denials; [`Self::validate`] refuses it until a path is denied.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Deny the subtree beneath `path`.
    pub fn deny(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.paths.push(path.as_ref().to_path_buf());
        self
    }

    /// The denied subtrees, in the order they were named, for inspection only.
    pub fn paths(&self) -> impl ExactSizeIterator<Item = &Path> {
        self.paths.iter().map(PathBuf::as_path)
    }

    /// Refuse a denial list that could not confine anything or that the
    /// wrapper could misread.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::InvalidInput`] for an empty list, one past
    /// [`Self::MAX_PATHS`], or a path that is relative, longer than
    /// [`Self::MAX_PATH_BYTES`], or holding a NUL byte.
    pub fn validate(&self) -> io::Result<()> {
        if let Some(message) = self.refusal() {
            lgwks_std::trace::warn!(%message, "DenyBeneath::validate refused a denial list before the fork");
            return Err(io::Error::new(io::ErrorKind::InvalidInput, message));
        }
        Ok(())
    }

    /// Why this list cannot be applied, or `None` when it can.
    fn refusal(&self) -> Option<String> {
        if self.paths.is_empty() || self.paths.len() > Self::MAX_PATHS {
            return Some(format!(
                "a denial list holds 1..={} paths, got {}",
                Self::MAX_PATHS,
                self.paths.len()
            ));
        }
        self.paths.iter().find_map(|path| {
            let long = path.as_os_str().len() > Self::MAX_PATH_BYTES;
            let nulled = path.as_os_str().as_encoded_bytes().contains(&0);
            (!path.is_absolute() || long || nulled).then(|| {
                format!(
                    "a denied path is absolute, at most {} bytes, with no NUL: {path:?}",
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
        Confinement::DenyBeneath(ref denied) => {
            denied.validate()?;
            deny(denied)
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

/// Deny `denied`'s subtrees however the platform allows: a sandbox profile
/// on macOS, an empty `tmpfs` overmounted on each subtree in a mount
/// namespace on Linux.
fn deny(denied: &DenyBeneath) -> io::Result<Command> {
    for path in denied.paths() {
        let resolved = std::fs::canonicalize(path).map_err(|source| {
            io::Error::new(
                source.kind(),
                format!(
                    "a denied subtree is resolved before it is overmounted: {}: {source}",
                    path.display()
                ),
            )
        })?;
        if !resolved.is_dir() {
            lgwks_std::trace::warn!(?path, "a denial names a non-directory");
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a denied path is a directory: {}", path.display()),
            ));
        }
    }
    deny_platform(denied)
}

/// Deny `denied`'s subtrees in a sandbox profile (macOS): allow by default,
/// deny reads beneath each resolved denial.
#[cfg(target_os = "macos")]
fn deny_platform(denied: &DenyBeneath) -> io::Result<Command> {
    let mut profile = SandboxProfile::allow_default();
    for path in denied.paths() {
        profile.deny_read_beneath(path)?;
    }
    profile.validate()?;
    sandbox(&profile)
}

/// Deny `denied`'s subtrees under an empty `tmpfs` in a mount namespace
/// (Linux): `bwrap` where the host ships it, else `unshare -m` with a shell
/// that overmounts each denial before it execs the target.
#[cfg(target_os = "linux")]
fn deny_platform(denied: &DenyBeneath) -> io::Result<Command> {
    if std::path::Path::new(BWRAP).exists() {
        return Ok(bwrap_deny(denied));
    }
    if std::path::Path::new(UNSHARE).exists() {
        return Ok(unshare_deny(denied));
    }
    lgwks_std::trace::warn!("no mount-namespace wrapper on this host");
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "lgwks_bot: denying a subtree on Linux needs bwrap or unshare",
    ))
}

/// Every other target has no denial wrapper.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn deny_platform(_denied: &DenyBeneath) -> io::Result<Command> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "lgwks_bot: denying a subtree is supported on macOS and Linux only",
    ))
}

/// The `bwrap` that overmounts denials, and the `unshare` fallback. Text, not
/// a `Path`: `Path::new` is not callable in a const on the pinned toolchain.
#[cfg(target_os = "linux")]
const BWRAP: &str = "/usr/bin/bwrap";

/// The `unshare` that opens a mount namespace when `bwrap` is absent.
#[cfg(target_os = "linux")]
const UNSHARE: &str = "/usr/bin/unshare";

/// `bwrap --bind / / --tmpfs <denied>... --`: the child sees its own root
/// except for the denied subtrees, which read as empty directories.
#[cfg(target_os = "linux")]
fn bwrap_deny(denied: &DenyBeneath) -> Command {
    let mut command = Command::new(BWRAP);
    command.arg("--bind").arg("/");
    command.arg("/");
    for path in denied.paths() {
        command.arg("--tmpfs").arg(path.as_os_str());
    }
    command.arg("--");
    command
}

/// `unshare -m sh -c <overmount, then exec>`: the shell the namespace opens
/// overmounts each denial from `LGWKS_DENY_<n>` before it execs the target.
///
/// The denied paths travel in the environment rather than in the script, so
/// no quoting can misread them. A mount the kernel refuses exits the shell
/// before the target runs, which is fail-closed: the lane never runs
/// unconfined, it fails.
#[cfg(target_os = "linux")]
fn unshare_deny(denied: &DenyBeneath) -> Command {
    const SCRIPT: &str = r#"index=0
while :; do
  eval "denied=\${LGWKS_DENY_$index-}"
  [ -z "$denied" ] && break
  if [ -e "$denied" ] || [ -L "$denied" ]; then
    mount -t tmpfs -o size=64M,mode=0755 tmpfs "$denied" || exit 99
  fi
  index=$((index+1))
done
exec "$@""#;
    let mut command = Command::new(UNSHARE);
    command.arg("-m");
    command.arg("sh");
    command.arg("-c");
    command.arg(SCRIPT);
    command.arg("lgwks-deny");
    for (index, path) in denied.paths().enumerate() {
        command.env(format!("LGWKS_DENY_{index}"), path.as_os_str());
    }
    command
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
