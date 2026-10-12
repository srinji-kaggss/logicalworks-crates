//! Process identity (pid plus start time) and orphaned-group reaping.

use std::io;

#[cfg(unix)]
use super::descendants::run_ps;
#[cfg(not(unix))]
use super::unsupported;
#[cfg(unix)]
use super::{
    DescendantSet, capture_descendants, invalid_pid, kill_process, kill_process_group,
    process_exists,
};

// ── Process identity and orphaned-group reaping ─────────────────────────────
//
// A supervisor that is itself killed with SIGKILL runs no destructor, so the
// process groups it started outlive it with nobody left who owns them. A
// successor can only reap them if the dead supervisor recorded *which* processes
// they were, and a pid alone does not say: after the leader exits the OS may hand
// its number to an unrelated process, and a successor that signals the number
// signals the stranger. So the record is the pid **and** the instant the OS says
// that pid's process started, and a successor signals only while both still
// match. A number reissued to another process has a different start instant, so
// it reads as reused and nothing is sent.

/// The longest start token a [`ProcessIdentity`] carries, in bytes.
///
/// Both readings this module produces fit in well under half of it (a Linux boot
/// id and a tick count, or `ps`'s ten-word start date), so the bound only refuses
/// a stored record that was damaged or written by something else, and keeps such
/// a record from making a successor allocate without limit.
pub const MAX_START_TOKEN_BYTES: usize = 128;

/// The prefix of a start instant read from `/proc` (Linux).
const PROC_SCHEME: &str = "proc:";

/// The prefix of a start instant read from `ps -o lstart`.
const PS_SCHEME: &str = "ps:";

/// The separator between a pid and its start token in the rendered form.
const IDENTITY_SEPARATOR: char = '/';

/// Which reading of the process table produced a start token.
///
/// A token is only comparable with another token from the same reading, so the
/// reading travels with it and a successor re-reads the start the same way the
/// record was made, rather than comparing a `/proc` tick count with a `ps` date
/// and calling the difference a reuse.
#[cfg(all(unix, feature = "process"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
enum StartScheme {
    /// Linux: the boot id and the start tick from `/proc/<pid>/stat`.
    Proc,
    /// `ps -o lstart`, read in UTC under the C locale.
    Ps,
}

/// A process, named by its pid and the instant the OS records it started.
///
/// The pair is what a successor needs to tell the process it was given from a
/// stranger that holds the same number later. It renders as `<pid>/<start>` and
/// parses back from that form, so it can be stored in a database row or a file
/// and read by a different process — including one started after the process
/// that recorded it was killed.
///
/// The start token is **opaque and host-local**: it compares equal only with a
/// token read on the same host by the same reading, and it orders nothing. On
/// Linux it is the boot id and the kernel's start tick, so it is exact and does
/// not survive a reboot as a false match. Elsewhere it is `ps`'s start date,
/// which has a resolution of one second: a pid the OS reissues within the same
/// second its previous holder started reads as the same process. Reissuing a
/// pid requires the allocator to wrap its whole range first, which no host does
/// within a second, and the limit is stated rather than assumed away.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ProcessIdentity {
    /// The process id.
    pid: i32,
    /// The start token, including the prefix naming the reading that made it.
    started: String,
}

impl ProcessIdentity {
    /// Rebuild an identity from a pid and the start token a previous
    /// [`identify_process`] reported.
    ///
    /// # Errors
    ///
    /// [`ProcessIdentityError`] when `pid` is not positive or `started` is not a
    /// start token this module produces: empty, longer than
    /// [`MAX_START_TOKEN_BYTES`], holding a byte outside printable ASCII, or
    /// carrying no known reading prefix. A record that fails here was damaged or
    /// written by something else, and reaping against it would be a guess.
    pub fn new(pid: i32, started: &str) -> Result<Self, ProcessIdentityError> {
        if pid <= 0 {
            let refusal = Err(ProcessIdentityError::NonPositivePid { pid });
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "ProcessIdentity::new: returning an error to the caller");
            return refusal;
        }
        validate_start(started)?;
        Ok(Self {
            pid,
            started: started.to_owned(),
        })
    }

    /// The process id.
    #[must_use]
    pub const fn pid(&self) -> i32 {
        self.pid
    }

    /// The start token, comparable only with one read on the same host.
    #[must_use]
    pub fn started(&self) -> &str {
        &self.started
    }

    /// The reading that produced the start token, decided by its prefix, which
    /// every constructor has already validated.
    #[cfg(all(unix, feature = "process"))]
    fn scheme(&self) -> StartScheme {
        if self.started.starts_with(PROC_SCHEME) {
            StartScheme::Proc
        } else {
            StartScheme::Ps
        }
    }
}

impl std::fmt::Display for ProcessIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{IDENTITY_SEPARATOR}{}", self.pid, self.started)
    }
}

impl std::str::FromStr for ProcessIdentity {
    type Err = ProcessIdentityError;

    /// Parse the `<pid>/<start>` form [`ProcessIdentity`]'s `Display` writes.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let Some((pid, started)) = text.split_once(IDENTITY_SEPARATOR) else {
            let refusal = Err(ProcessIdentityError::MissingSeparator);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "ProcessIdentity::from_str: returning an error to the caller");
            return refusal;
        };
        // Only the digits `Display` writes: `i32::from_str` also takes a sign and
        // leading zeros, and a record read back must render as the text it was.
        let canonical = !pid.is_empty()
            && pid.bytes().all(|byte| byte.is_ascii_digit())
            && (pid == "0" || !pid.starts_with('0'));
        let Some(pid) = canonical.then(|| pid.parse::<i32>().ok()).flatten() else {
            let refusal = Err(ProcessIdentityError::MalformedPid);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "ProcessIdentity::from_str: returning an error to the caller");
            return refusal;
        };
        Self::new(pid, started)
    }
}

/// Why a stored [`ProcessIdentity`] was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProcessIdentityError {
    /// The text had no `/` between a pid and a start token.
    MissingSeparator,
    /// The pid was not an `i32` written as plain decimal digits.
    MalformedPid,
    /// The pid was zero or negative; neither names one process.
    NonPositivePid {
        /// The pid that was refused.
        pid: i32,
    },
    /// The start token was empty.
    EmptyStart,
    /// The start token was longer than [`MAX_START_TOKEN_BYTES`].
    StartTooLong {
        /// Its length in bytes.
        len: usize,
    },
    /// The start token held a byte outside printable ASCII.
    StartNotPrintable {
        /// The byte offset of the first such byte.
        at: usize,
    },
    /// The start token named no reading this module produces.
    UnknownScheme,
}

impl std::fmt::Display for ProcessIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::MissingSeparator => f.write_str("a process identity needs `<pid>/<start>`"),
            Self::MalformedPid => f.write_str("a process identity's pid is not a decimal i32"),
            Self::NonPositivePid { pid } => {
                write!(f, "a process identity's pid must be positive, got {pid}")
            }
            Self::EmptyStart => f.write_str("a process identity's start token is empty"),
            Self::StartTooLong { len } => write!(
                f,
                "a process identity's start token is {len} bytes, past the {MAX_START_TOKEN_BYTES}-byte bound"
            ),
            Self::StartNotPrintable { at } => write!(
                f,
                "a process identity's start token holds a non-printable byte at offset {at}"
            ),
            Self::UnknownScheme => {
                f.write_str("a process identity's start token names no reading this crate produces")
            }
        }
    }
}

impl std::error::Error for ProcessIdentityError {}

/// Refuse a start token this module could not have produced.
fn validate_start(started: &str) -> Result<(), ProcessIdentityError> {
    let refusal = if started.is_empty() {
        Some(ProcessIdentityError::EmptyStart)
    } else if started.len() > MAX_START_TOKEN_BYTES {
        Some(ProcessIdentityError::StartTooLong { len: started.len() })
    } else {
        started
            .bytes()
            .position(|byte| !byte.is_ascii_graphic())
            .map(|at| ProcessIdentityError::StartNotPrintable { at })
    };
    if let Some(error) = refusal {
        let refusal = Err(error);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "validate_start: returning an error to the caller");
        return refusal;
    }
    if started.starts_with(PROC_SCHEME) || started.starts_with(PS_SCHEME) {
        Ok(())
    } else {
        let refusal = Err(ProcessIdentityError::UnknownScheme);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "validate_start: returning an error to the caller");
        refusal
    }
}

/// The identity of the process `pid` names now, or `None` when no process holds it.
///
/// Call it while the process is known to be the one meant — for a supervisor,
/// before its child is reaped — and store the result; [`reap_orphaned_group`]
/// later compares it with whoever holds the number then. On Linux it reads
/// `/proc` (the boot id and the start tick) and falls back to `ps` when `/proc`
/// cannot be read; elsewhere it runs `ps -o lstart -p <pid>` once, in UTC under
/// the C locale so two readers in different time zones read the same token.
///
/// # Errors
///
/// [`std::io::ErrorKind::InvalidInput`] for a non-positive `pid`, and the
/// reading's own error when the table could not be read — never `None` for a
/// reading that failed, because "no such process" and "could not look" are
/// different answers to a caller about to signal.
#[cfg(all(unix, feature = "process"))]
pub fn identify_process(pid: i32) -> io::Result<Option<ProcessIdentity>> {
    if pid <= 0 {
        let refusal = Err(invalid_pid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "identify_process: returning an error to the caller");
        return refusal;
    }
    #[cfg(target_os = "linux")]
    let started = match proc_start(pid) {
        Ok(started) => started,
        Err(error) => {
            #[cfg(feature = "trace")]
            crate::trace::debug!(pid, error = %error, "identify_process: /proc was unreadable; reading ps");
            #[cfg(not(feature = "trace"))]
            let _unreported = error;
            ps_start(pid)?
        }
    };
    #[cfg(not(target_os = "linux"))]
    let started = ps_start(pid)?;
    Ok(started.map(|started| ProcessIdentity { pid, started }))
}

/// The identity of the process `pid` names now.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`].
#[cfg(all(not(unix), feature = "process"))]
pub fn identify_process(pid: i32) -> io::Result<Option<ProcessIdentity>> {
    let _ = pid;
    Err(unsupported("process identify_process is Unix-only"))
}

/// What [`reap_orphaned_group`] found and did.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OrphanReap {
    /// The recorded leader still held its pid, so its group, the leader itself
    /// and every descendant captured from it were sent `SIGKILL`.
    ///
    /// A member that had already exited when its signal was sent counts as
    /// stopped: the signal's purpose was met by the exit.
    Signalled {
        /// The descendants captured from the leader before the group signal,
        /// with the mechanism that found them.
        descendants: DescendantSet,
    },
    /// No process holds the recorded pid, so nothing was signalled.
    ///
    /// The leader is gone. Members of its group that outlived it are not
    /// reachable by this record: a group whose leader has exited can no longer
    /// be told apart from a later group under the same number, so it is left
    /// alone rather than guessed at.
    LeaderGone,
    /// Another process holds the recorded pid now, so nothing was signalled.
    LeaderReused {
        /// The identity of the process that holds the number now.
        holder: ProcessIdentity,
    },
}

/// Stop the process group `leader` led, if `leader` is still the process holding
/// its pid.
///
/// The successor's half of [`identify_process`]: a supervisor that was killed
/// before it could clean up leaves its children's groups running, and a later
/// process holding the identities it recorded calls this once per identity.
/// The current holder of the pid is read the way the record was made; only an
/// exact match is signalled, and then the leader's descendants are captured,
/// the group is killed, the leader is killed by pid in case it left its group,
/// and every captured descendant is killed by pid, which reaches one that called
/// `setsid`.
///
/// **Not claimed:** atomicity. The read and the signal are separate calls, so a
/// leader that exits between them and whose pid the OS reissues in that window
/// would be signalled under its successor's name; reissuing a pid requires the
/// allocator to wrap its whole range first. A group whose leader has already
/// gone is not reached at all (see [`OrphanReap::LeaderGone`]).
///
/// # Errors
///
/// The reading's own error when the current holder could not be identified,
/// [`std::io::ErrorKind::Unsupported`] for a `/proc` record read on a host
/// without `/proc`, and a signal's own error other than "no such process" —
/// `EPERM`, for one, says the group belongs to someone this process may not
/// signal, which is a fact the caller must see.
#[cfg(all(unix, feature = "process"))]
pub fn reap_orphaned_group(leader: &ProcessIdentity) -> io::Result<OrphanReap> {
    let holder = match leader.scheme() {
        StartScheme::Ps => {
            // The probe before the spawn (#345): `ps_start` answers `None`
            // exactly when no process holds the pid — it disambiguates an
            // empty `ps` listing with this same probe — so an absent pid is
            // `LeaderGone` without spawning `ps` to learn nothing.
            if !process_exists(leader.pid)? {
                return Ok(OrphanReap::LeaderGone);
            }
            ps_start(leader.pid)?
        }
        #[cfg(target_os = "linux")]
        StartScheme::Proc => proc_start(leader.pid)?,
        #[cfg(not(target_os = "linux"))]
        StartScheme::Proc => {
            let refusal = Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a /proc process identity can only be read on Linux",
            ));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "reap_orphaned_group: returning an error to the caller");
            return refusal;
        }
    };
    let Some(started) = holder else {
        return Ok(OrphanReap::LeaderGone);
    };
    if started != leader.started {
        return Ok(OrphanReap::LeaderReused {
            holder: ProcessIdentity {
                pid: leader.pid,
                started,
            },
        });
    }
    let descendants = match capture_descendants(leader.pid) {
        Ok(captured) => captured,
        Err(error) => {
            // The group signal below still reaches every member that stayed in
            // the group; the empty capture says no table was read.
            #[cfg(feature = "trace")]
            crate::trace::debug!(pid = leader.pid, error = %error, "reap_orphaned_group: the descendants could not be captured");
            #[cfg(not(feature = "trace"))]
            let _unreported = error;
            DescendantSet::default()
        }
    };
    stopped_unless_refused(kill_process_group(leader.pid))?;
    stopped_unless_refused(kill_process(leader.pid))?;
    for pid in descendants.pids() {
        stopped_unless_refused(kill_process(*pid))?;
    }
    Ok(OrphanReap::Signalled { descendants })
}

/// Stop the process group `leader` led, if `leader` is still the process holding
/// its pid.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`].
#[cfg(all(not(unix), feature = "process"))]
pub fn reap_orphaned_group(leader: &ProcessIdentity) -> io::Result<OrphanReap> {
    let _ = leader;
    Err(unsupported("process reap_orphaned_group is Unix-only"))
}

/// A signal answered "no such process" reached a target that had already
/// stopped, which is what the signal was for; every other refusal stands.
#[cfg(all(unix, feature = "process"))]
fn stopped_unless_refused(signalled: io::Result<()>) -> io::Result<()> {
    match signalled {
        Err(error) if error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) => {
            Ok(())
        }
        other => other,
    }
}

/// The `/proc` start token of `pid`: the boot id and the kernel's start tick.
///
/// `Ok(None)` when `/proc/<pid>` does not exist; an error when the boot id or the
/// stat line cannot be read or parsed, which the caller treats as "could not
/// look" rather than as absence.
#[cfg(all(target_os = "linux", feature = "process"))]
fn proc_start(pid: i32) -> io::Result<Option<String>> {
    // `starttime` is field 22 of `stat`; counted from the state letter (field
    // 3), the first field after the command name, it is the twentieth.
    const START_FIELD_AFTER_NAME: usize = 19;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let stat_path = std::path::Path::new("/proc")
        .join(pid.to_string())
        .join("stat");
    let stat = match std::fs::read_to_string(stat_path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            let refusal = Err(error);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "proc_start: returning an error to the caller");
            return refusal;
        }
    };
    // `comm` may itself contain spaces and parentheses, so the fields start
    // after the **last** `)`.
    let ticks = stat
        .rfind(')')
        .and_then(|end| stat.get(end.saturating_add(1)..))
        .and_then(|fields| fields.split_ascii_whitespace().nth(START_FIELD_AFTER_NAME));
    let Some(ticks) = ticks else {
        let refusal = Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("/proc/{pid}/stat carried no start tick"),
        ));
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "proc_start: returning an error to the caller");
        return refusal;
    };
    Ok(Some(format!("{PROC_SCHEME}{}:{ticks}", boot.trim())))
}

/// The `ps -o lstart` start token of `pid`, read in UTC under the C locale.
///
/// `ps` prints nothing for a pid it does not list, which is either absence or a
/// failed read; signal zero decides which, so a table that could not be read is
/// never reported as an absent process.
#[cfg(all(unix, feature = "process"))]
fn ps_start(pid: i32) -> io::Result<Option<String>> {
    let pid_text = pid.to_string();
    let output = run_ps(&["-o", "lstart=", "-p", &pid_text], "ps_start")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let words: Vec<&str> = text.split_ascii_whitespace().collect();
    if words.is_empty() {
        if process_exists(pid)? {
            let refusal = Err(io::Error::other(format!(
                "lgwks_std::process (ps_start): `ps` listed no start for live pid {pid}"
            )));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "ps_start: returning an error to the caller");
            return refusal;
        }
        return Ok(None);
    }
    Ok(Some(format!("{PS_SCHEME}{}", words.join("-"))))
}
