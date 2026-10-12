//! Kernel-level containment: cgroup v2 and the child subreaper (Linux).
//!
//! A process group is the floor: a descendant that calls `setsid` leaves it by
//! construction, and a capture is a snapshot that races the fork it is trying to
//! name. The two kernel owners below close those gaps without a new dependency,
//! using only `std::fs` and the `rustix/process` calls this module already
//! makes:
//!
//! - a cgroup v2 scope the supervisor creates per tree: every member, including
//!   a `setsid` escapee attached after the spawn, dies on one `cgroup.kill`
//!   write, atomically, with no fork race for the members it holds;
//! - the child subreaper flag plus an attribution-checked reap: an orphan whose
//!   parent died before the cleanup is re-parented to *this* process instead of
//!   init, and a pid that is both a captured descendant and a current child of
//!   this process is provably still the same process (an unreaped child keeps
//!   its pid unreissued), so it is safe to signal where a re-capture from a
//!   reaped leader would not be.
//!
//! Where neither is available the supervisor falls back to the group and the
//! capture, and the receipt says so. Nothing here is claimed on non-Linux
//! targets: other targets report [`std::io::ErrorKind::Unsupported`].

use std::collections::BTreeSet;
use std::io;

use super::capture::read_proc_children;
use super::{errno_to_io, invalid_pid};

/// The cgroup v2 mount this module scopes trees under.
///
/// A child cgroup is created per supervised tree, so one tree's kill never
/// touches another's members. The mount is read once per scope rather than
/// cached in a global: a container that gains delegation after the first spawn
/// must be usable by the second, and a global would freeze the first answer.
#[cfg(all(target_os = "linux", feature = "process"))]
const CGROUP_V2_MOUNT: &str = "/sys/fs/cgroup";

/// The longest scope name [`CgroupScope::create`] accepts, in bytes.
///
/// A scope name is a single directory entry under the v2 mount, so it must fit
/// one; the bound refuses a caller-built path rather than truncating it into a
/// name that collides with another tree's.
#[cfg(all(target_os = "linux", feature = "process"))]
const MAX_SCOPE_NAME_BYTES: usize = 64;

/// One supervised tree's cgroup v2 scope: its members die on one write.
///
/// Created per tree by the supervisor after the fork and removed when the scope
/// drops. Membership is inherited across `fork`, so a grandchild forked after
/// its parent was attached is a member too; a descendant forked in the window
/// between the spawn and the attach is not, which is why the supervisor keeps
/// its capture-and-signal rounds beside this rather than replacing them. The
/// kill is synchronous as far as the writer is concerned — the write returns
/// after every member was sent `SIGKILL` — and the supervisor still observes
/// the scope empty afterwards rather than trusting the return.
///
/// A scope whose supervisor was itself killed with `SIGKILL` runs no
/// destructor: its directory outlives it on the kernel's filesystem until the
/// host clears it. The directory is empty of members (the kill ran first, and
/// a member keeps its directory busy), so the residue is one empty directory,
/// never a running process.
#[cfg(all(target_os = "linux", feature = "process"))]
#[derive(Debug)]
pub struct CgroupScope {
    /// The scope directory; removed best-effort on drop.
    path: std::path::PathBuf,
}

#[cfg(all(target_os = "linux", feature = "process"))]
impl CgroupScope {
    /// Create the scope `name` under the cgroup v2 mount.
    ///
    /// `name` is one directory entry — alphanumeric, `-`, `_`, `.` and `+`,
    /// and neither `.` nor `..` — so a caller cannot smuggle a path into
    /// another tree's scope, the mount, or its parent. An unwritable or
    /// absent mount is
    /// [`std::io::ErrorKind::Unsupported`]: the supervisor treats that as "use
    /// the subreaper", not as a failure, because a container without delegation
    /// is a fact about the host rather than a refusal of this tree. Any other
    /// OS error stands.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::InvalidInput`] for a name that is empty, too long,
    /// names `.` or `..`, or carries a byte outside the allowed set;
    /// [`std::io::ErrorKind::Unsupported`] when no child cgroup is creatable;
    /// the creation's own error otherwise.
    pub fn create(name: &str) -> io::Result<Self> {
        validate_scope_name(name)?;
        let path = std::path::Path::new(CGROUP_V2_MOUNT).join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => Ok(Self { path }),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || error.kind() == io::ErrorKind::PermissionDenied
                    || error.kind() == io::ErrorKind::ReadOnlyFilesystem =>
            {
                let refusal = Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "lgwks_std::process (CgroupScope::create): no child cgroup is creatable under {CGROUP_V2_MOUNT} ({}), so this host has no cgroup containment",
                        error.kind(),
                    ),
                ));
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::create: returning an error to the caller");
                refusal
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                // A scope from a supervisor that died without running its
                // destructor — or a name colliding with another live tree.
                // Reused only when it holds no members: a scope that still
                // holds processes belongs to whoever put them there, and this
                // tree must not attach to it (its kill would stop them) nor
                // claim its receipt. An unreadable member list refuses the
                // same way: what cannot be shown empty is not empty.
                let scope = Self { path };
                match scope.members() {
                    Ok(members) if members.is_empty() => Ok(scope),
                    Ok(members) => {
                        let refusal = Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            format!(
                                "lgwks_std::process (CgroupScope::create): the scope holds {} processes of another tree, so this tree runs without a scope",
                                members.len(),
                            ),
                        ));
                        #[cfg(feature = "trace")]
                        crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::create: returning an error to the caller");
                        refusal
                    }
                    Err(error) => {
                        let refusal = Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            format!(
                                "lgwks_std::process (CgroupScope::create): the scope's members are unreadable ({error}), so this tree runs without a scope",
                            ),
                        ));
                        #[cfg(feature = "trace")]
                        crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::create: returning an error to the caller");
                        refusal
                    }
                }
            }
            Err(error) => {
                let refusal = Err(error);
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::create: returning an error to the caller");
                refusal
            }
        }
    }

    /// The scope directory, for inspection only.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Attach `pid` to this scope.
    ///
    /// The supervisor calls this with the pid its spawn just returned, before
    /// the child is awaited: every descendant forked afterwards inherits the
    /// membership. `pid` must be positive; a non-positive id is refused before
    /// anything is written.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::InvalidInput`] for a non-positive `pid`, and the
    /// write's own error otherwise — including the case where `pid` exited
    /// between the spawn and the attach, which the supervisor reads as "this
    /// tree needs the capture rounds, not the scope".
    pub fn add(&self, pid: i32) -> io::Result<()> {
        if pid <= 0 {
            let refusal = Err(invalid_pid());
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::add: returning an error to the caller");
            return refusal;
        }
        match std::fs::write(self.path.join("cgroup.procs"), pid.to_string()) {
            Ok(()) => Ok(()),
            Err(error) => {
                let refusal = Err(error);
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::add: returning an error to the caller");
                refusal
            }
        }
    }

    /// Send `SIGKILL` to every member of this scope, atomically.
    ///
    /// This is `cgroup.kill`: one write reaches every member, including one
    /// that called `setsid` after it was attached, with no fork race for the
    /// members the scope holds. It is the supervisor's first signal, before its
    /// capture rounds; what it could not hold (a descendant forked before the
    /// attach) the rounds still cover.
    ///
    /// # Errors
    ///
    /// The write's own error. A refused write leaves the scope untouched and
    /// the supervisor falls back to its rounds.
    pub fn kill(&self) -> io::Result<()> {
        match std::fs::write(self.path.join("cgroup.kill"), "1") {
            Ok(()) => Ok(()),
            Err(error) => {
                let refusal = Err(error);
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::kill: returning an error to the caller");
                refusal
            }
        }
    }

    /// The pids currently holding membership in this scope, sorted.
    ///
    /// The supervisor reads this after [`Self::kill`] to earn its receipt: an
    /// empty scope is the kernel's own confirmation that nothing of the tree
    /// is left, and a non-empty one names the pids the rounds must still
    /// account for. Unparseable rows are skipped — a row this reader cannot
    /// parse names no process it could signal — and an unreadable scope is the
    /// read's own error, never an empty scope.
    ///
    /// # Errors
    ///
    /// The read's own error when the scope's member list cannot be read at all.
    pub fn members(&self) -> io::Result<Vec<i32>> {
        match std::fs::read_to_string(self.path.join("cgroup.procs")) {
            Ok(text) => Ok(text
                .split_ascii_whitespace()
                .filter_map(|pid| pid.parse::<i32>().ok())
                .filter(|pid| *pid > 0)
                .collect()),
            Err(error) => {
                let refusal = Err(error);
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::members: returning an error to the caller");
                refusal
            }
        }
    }
}

#[cfg(all(target_os = "linux", feature = "process"))]
impl Drop for CgroupScope {
    /// Remove the scope directory, best effort.
    ///
    /// A scope that still holds members refuses the removal and keeps them:
    /// the drop never kills, so a forgotten scope is an empty directory at
    /// worst, never a signalled tree. Errors are ignored because a destructor
    /// has nowhere truthful to report them.
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir(&self.path);
    }
}

/// Refuse a scope name that is not one safe directory entry.
#[cfg(all(target_os = "linux", feature = "process"))]
fn validate_scope_name(name: &str) -> io::Result<()> {
    // `.` names the mount itself and `..` its parent: accepting either would
    // attach trees to — and `kill()` — a scope this supervisor does not own,
    // up to every process on the host. Both are refused as input, never
    // resolved.
    let valid = !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= MAX_SCOPE_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'));
    if valid {
        Ok(())
    } else {
        let refusal = Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a cgroup scope name is one directory entry of letters, digits, '-', '_', '.' or '+', and neither '.' nor '..'",
        ));
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "validate_scope_name: returning an error to the caller");
        refusal
    }
}

/// The pids currently holding this process as their parent, sorted.
///
/// The subreaper flag makes this the adoptee list: every orphan re-parented
/// here appears beside the direct children this process forked itself. The
/// caller tells the two apart by intersecting with what it captured — a pid
/// it never named is somebody else's child, not an adoptee it may signal.
///
/// # Errors
///
/// The child-list read's own error, in which case nothing is claimed about
/// who this process parents.
#[cfg(all(target_os = "linux", feature = "process"))]
pub fn own_children() -> io::Result<Vec<i32>> {
    let own = i32::try_from(std::process::id()).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("own_children: the supervisor's own pid does not fit i32: {error}"),
        )
    })?;
    match read_proc_children(own) {
        Ok(mut children) => {
            children.sort_unstable();
            Ok(children)
        }
        Err(error) => {
            let refusal = Err(error);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "own_children: returning an error to the caller");
            refusal
        }
    }
}

/// Make orphaned descendants re-parent to this process instead of init.
///
/// This is `prctl(PR_SET_CHILD_SUBREAPER)`: a descendant whose parent exits
/// before the supervisor's cleanup is adopted by this process rather than by
/// the platform's init, so it stays findable — and attributable, which init's
/// adoptees are not. Idempotent and process-wide: setting it twice changes
/// nothing, and every supervisor in this process shares the adoptees, which is
/// why [`adopted_descendants`] intersects them with each tree's own capture
/// rather than trusting parenthood alone.
///
/// A supervisor calls this once before its first spawn; the flag survives
/// `exec` of nothing here (this process never execs) and needs no renewal.
///
/// # Errors
///
/// The `prctl` call's own error. A refused flag leaves re-parenting to init,
/// and the supervisor falls back to the group and the capture.
#[cfg(all(target_os = "linux", feature = "process"))]
pub fn enable_child_subreaper() -> io::Result<()> {
    // `PR_SET_CHILD_SUBREAPER` takes a flag, and rustix spells the flag as the
    // adopter's pid: `None` is zero, which *clears* the setting. Passing this
    // process's own pid sets it with the only adopter this supervisor can name.
    let own_pid = i32::try_from(std::process::id()).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("enable_child_subreaper: the supervisor's own pid does not fit i32: {error}"),
        )
    })?;
    let own = rustix::process::Pid::from_raw(own_pid).ok_or_else(invalid_pid)?;
    match rustix::process::set_child_subreaper(Some(own)) {
        Ok(()) => Ok(()),
        Err(errno) => {
            let refusal = Err(errno_to_io(errno));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "enable_child_subreaper: returning an error to the caller");
            refusal
        }
    }
}

/// Which of `candidates` are currently children of this process.
///
/// The attribution check that makes the subreaper safe to signal through: a
/// pid that is both a descendant this supervisor captured while its leader
/// lived *and* a current, unreaped child of this process is provably still the
/// same process — the OS cannot reissue the pid of a child nobody reaped — so
/// signalling it cannot reach a stranger. A candidate that is not a child of
/// this process is either still under its parent (and covered by the rounds)
/// or was reaped already (and gone); either way it is left alone here.
///
/// The answer is about this instant: a pid adopted after the call is not in
/// it, and the caller re-asks rather than reuses.
///
/// # Errors
///
/// [`std::io::ErrorKind::InvalidInput`] is never produced here — candidates
/// are not validated, non-positive ones simply never match a child — and the
/// only refusal is the child-list read's own error, in which case nothing is
/// claimed adopted.
#[cfg(all(target_os = "linux", feature = "process"))]
pub fn adopted_descendants(candidates: &[i32]) -> io::Result<BTreeSet<i32>> {
    let self_pid = std::process::id();
    let own = match read_proc_children(i32::try_from(self_pid).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "adopted_descendants: the supervisor's own pid {self_pid} does not fit i32: {error}"
            ),
        )
    })?) {
        Ok(children) => children,
        Err(error) => {
            let refusal = Err(error);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "adopted_descendants: returning an error to the caller");
            return refusal;
        }
    };
    let own: BTreeSet<i32> = own.into_iter().collect();
    Ok(candidates
        .iter()
        .copied()
        .filter(|pid| own.contains(pid))
        .collect())
}

/// Reap each of `pids` that is a zombie child of this process.
///
/// The targeted counterpart of the orphan queue: `waitpid` on the exact pid,
/// so a live supervised leader is never stolen by a reap meant for an adopted
/// orphan. A pid that already exited is collected; one still running is left
/// alone; and one that is not, or is no longer, a child of this process —
/// reaped by another waiter first — is passed over without being named, so it
/// is absent from the answer exactly as a running one is. A caller that must
/// tell those two apart asks [`adopted_descendants`] which pids it still holds.
///
/// Returns the pids this call reaped, sorted.
///
/// # Errors
///
/// The wait's own error other than `ECHILD` and `ESRCH`, the two answers for a
/// pid that is not this process's to wait for.
#[cfg(all(target_os = "linux", feature = "process"))]
pub fn reap_descendants(pids: &[i32]) -> io::Result<Vec<i32>> {
    let mut reaped = Vec::new();
    for pid in pids {
        let Some(target) = rustix::process::Pid::from_raw(*pid) else {
            continue;
        };
        match rustix::process::waitpid(Some(target), rustix::process::WaitOptions::NOHANG) {
            Ok(Some(_)) => reaped.push(*pid),
            Ok(None) => {}
            Err(rustix::io::Errno::CHILD) | Err(rustix::io::Errno::SRCH) => {}
            Err(errno) => {
                let refusal = Err(errno_to_io(errno));
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "reap_descendants: returning an error to the caller");
                return refusal;
            }
        }
    }
    reaped.sort_unstable();
    Ok(reaped)
}
