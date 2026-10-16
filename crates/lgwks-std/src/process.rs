//! Process-group termination and observation for supervised subprocesses.
//!
//! Stable `std` can kill a direct child (`Child::kill`) but cannot signal a
//! whole process group, so killing a shell leaves orphaned grandchildren.
//! It also cannot observe a child's exit without reaping it, which is the
//! primitive a supervisor needs to keep a group leader's id from being
//! reused under it: a zombie leader still holds its pid, so its group id
//! cannot be reissued until the supervisor has finished signalling. These
//! are the `process` feature's primitives. Unix and Windows; other targets
//! report [`std::io::ErrorKind::Unsupported`] rather than a fabricated success.
//! Windows has no process groups, so a "group" there is the tree below a root
//! process (`kill_process_group` stops the root and everything the process
//! table names as descended from it), and `ProcessJob` is its kernel-level
//! owner, the counterpart of a cgroup.
//!
//! The module is split by responsibility: this file holds the group and child
//! primitives, `capture` the descendant capture and the running check,
//! `identity` process identities and orphaned-group reaping, `cgroup` the
//! Linux kernel-level containment, `darwin` the one macOS process-table read
//! (a `sysctl` syscall) and `windows` the `kernel32` calls (the only `unsafe` in
//! the workspace, #345, #263).

use std::io;

/// Descendant capture, the running check, and the process-table readings.
mod capture;
/// cgroup v2 scopes and the child subreaper (Linux).
#[cfg(target_os = "linux")]
mod cgroup;
/// The macOS process table through one `sysctl(KERN_PROC)` syscall (#345).
#[cfg(target_os = "macos")]
mod darwin;
/// Process identities and orphaned-group reaping.
mod identity;
/// Windows process table, identity, termination and job objects (#263).
#[cfg(windows)]
mod windows;

pub use capture::{
    ContainmentMechanism, DescendantSet, MAX_CAPTURED_DESCENDANTS, capture_descendants,
    kill_process, process_exists, running_processes,
};
#[cfg(target_os = "linux")]
pub use cgroup::{
    CgroupScope, adopted_descendants, enable_child_subreaper, own_children, reap_descendants,
};
pub use identity::{
    MAX_START_TOKEN_BYTES, OrphanReap, ProcessIdentity, ProcessIdentityError, identify_process,
    reap_orphaned_group,
};
#[cfg(windows)]
pub use windows::ProcessJob;

/// Send SIGKILL to every process in group `pgid`.
///
/// `pgid` is a process-group id as reported by the OS (a positive integer;
/// typically the leader's pid). An invalid id surfaces the OS error rather than
/// succeeding silently. This kills unconditionally (SIGKILL cannot be
/// caught); callers that need graceful shutdown must signal SIGTERM
/// themselves before falling back here.
#[cfg(all(unix, feature = "process"))]
pub fn kill_process_group(pgid: i32) -> io::Result<()> {
    // Reject before touching rustix: 0 names the CALLER's group (SIGKILL
    // would hit us) and negatives panic inside Pid::from_raw in debug.
    if pgid <= 0 {
        let refusal = Err(invalid_pgid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "kill_process_group: returning an error to the caller");
        return refusal;
    }
    let pid = pid_from_raw(pgid)?;
    rustix::process::kill_process_group(pid, rustix::process::Signal::KILL).map_err(errno_to_io)
}

/// Stop the process `pgid` and every process descended from it.
///
/// Windows has no process groups, so the group is the tree: the descendants the
/// process table names under `pgid` are terminated, then `pgid` itself. A tree
/// that left through a parent that had already exited is not reachable from the
/// root; [`ProcessJob`] is the owner that reaches it. An id no process holds is
/// an error (`ERROR_INVALID_PARAMETER`, the Windows form of `ESRCH`).
#[cfg(all(windows, feature = "process"))]
pub fn kill_process_group(pgid: i32) -> io::Result<()> {
    let root = windows::pid_u32(pgid)?;
    for pid in capture_descendants(pgid)?.pids() {
        match windows::terminate(&windows::NATIVE, windows::pid_u32(*pid)?) {
            Err(error) if error.raw_os_error() == Some(windows::ERROR_INVALID_PARAMETER) => {}
            other => other?,
        }
    }
    windows::terminate(&windows::NATIVE, root)
}

/// Send SIGKILL to every process in group `pgid`.
///
/// The `process` capability is Unix and Windows; other targets report
/// [`std::io::ErrorKind::Unsupported`] rather than a fabricated success.
#[cfg(all(not(any(unix, windows)), feature = "process"))]
pub fn kill_process_group(pgid: i32) -> io::Result<()> {
    let _ = pgid;
    Err(unsupported("process kill_process_group is Unix-only"))
}

/// Whether `pgid` currently names a process group.
///
/// This is `kill(-pgid, 0)`: signal zero checks the group without delivering
/// anything to it. `EPERM` means the group exists but this process may not
/// signal it, so it counts as present; `ESRCH` means no process is in it. A
/// supervisor confirms a cleanup with this after `kill_process_group`, so a
/// group that cannot be observed is reported as an error, never as absent.
///
/// `pgid` must be positive: 0 names the caller's own group and a negative id
/// names a process, so both are refused as [`std::io::ErrorKind::InvalidInput`].
#[cfg(all(unix, feature = "process"))]
pub fn process_group_exists(pgid: i32) -> io::Result<bool> {
    if pgid <= 0 {
        let refusal = Err(invalid_pgid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "process_group_exists: returning an error to the caller");
        return refusal;
    }
    let pid = pid_from_raw(pgid)?;
    match rustix::process::test_kill_process_group(pid) {
        Ok(()) | Err(rustix::io::Errno::PERM) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(errno) => Err(errno_to_io(errno)),
    }
}

/// Whether the tree under `pgid` still has a process: the root holds its id, or
/// a process the table names as its descendant is running.
#[cfg(all(windows, feature = "process"))]
pub fn process_group_exists(pgid: i32) -> io::Result<bool> {
    let root = windows::pid_u32(pgid)?;
    if windows::exists(&windows::NATIVE, root)? {
        return Ok(true);
    }
    Ok(!running_processes(capture_descendants(pgid)?.pids())?.is_empty())
}

/// Whether `pgid` currently names a process group.
///
/// The `process` capability is Unix and Windows; other targets report
/// [`std::io::ErrorKind::Unsupported`] rather than a fabricated answer.
#[cfg(all(not(any(unix, windows)), feature = "process"))]
pub fn process_group_exists(pgid: i32) -> io::Result<bool> {
    let _ = pgid;
    Err(unsupported("process process_group_exists is Unix-only"))
}

/// Whether child `pid` has exited, leaving it un-reaped.
///
/// This is `waitid(P_PID, pid, WEXITED | WNOHANG | WNOWAIT)`: it reports the
/// exit and *leaves the child in the waitable state*, so a following
/// `Child::wait` still returns the real exit status. That gap — exited but
/// not reaped — is what a supervisor needs: an unreaped (zombie) leader
/// still occupies its pid, so the group id cannot be reused by the OS while
/// the supervisor still owes signals to it. Reaping is what releases the id,
/// and this call never reaps.
///
/// `Ok(false)` means still running; `Ok(true)` means exited and still
/// waitable.
#[cfg(all(unix, feature = "process"))]
pub fn child_has_exited_without_reaping(pid: i32) -> io::Result<bool> {
    if pid <= 0 {
        let refusal = Err(invalid_pid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "child_has_exited_without_reaping: returning an error to the caller");
        return refusal;
    }
    let Some(target) = rustix::process::Pid::from_raw(pid) else {
        let refusal = Err(invalid_pid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "child_has_exited_without_reaping: returning an error to the caller");
        return refusal;
    };
    let observed = rustix::process::waitid(
        rustix::process::WaitId::Pid(target),
        rustix::process::WaitIdOptions::EXITED
            | rustix::process::WaitIdOptions::NOHANG
            | rustix::process::WaitIdOptions::NOWAIT,
    )
    .map_err(errno_to_io)?;
    Ok(observed.is_some())
}

/// Whether process `pid` has exited while something still holds its object.
///
/// A held process object keeps the id from being reissued, which is the Windows
/// form of an unreaped child. An id no process holds is an error, never `true`.
#[cfg(all(windows, feature = "process"))]
pub fn child_has_exited_without_reaping(pid: i32) -> io::Result<bool> {
    windows::exited(&windows::NATIVE, windows::pid_u32(pid)?)
}

/// Whether child `pid` has exited, leaving it un-reaped.
///
/// The `process` capability is Unix and Windows; other targets report
/// [`std::io::ErrorKind::Unsupported`] rather than a fabricated success.
#[cfg(all(not(any(unix, windows)), feature = "process"))]
pub fn child_has_exited_without_reaping(pid: i32) -> io::Result<bool> {
    let _ = pid;
    Err(unsupported(
        "process child_has_exited_without_reaping is Unix-only",
    ))
}

#[cfg(all(unix, feature = "process"))]
/// Convert a rustix errno into the std error surface.
fn errno_to_io(errno: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(errno.raw_os_error())
}

#[cfg(all(unix, feature = "process"))]
/// Convert a positive raw process-group id into rustix's checked pid type.
fn pid_from_raw(pgid: i32) -> io::Result<rustix::process::Pid> {
    rustix::process::Pid::from_raw(pgid).ok_or_else(invalid_pgid)
}

#[cfg(all(unix, feature = "process"))]
/// Construct the invalid-process-group-id error.
fn invalid_pgid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "process group id must be a positive pid",
    )
}

#[cfg(all(any(unix, windows), feature = "process"))]
/// Construct the invalid-process-id error.
fn invalid_pid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "process id must be a positive pid",
    )
}

#[cfg(all(not(any(unix, windows)), feature = "process"))]
/// Construct the unsupported-operation error for non-Unix targets.
fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

// ── Tests ───────────────────────────────────────────────────────────────────

/// The module's unit tests, kept at `lgwks_std::process::tests`.
#[cfg(test)]
mod tests;
