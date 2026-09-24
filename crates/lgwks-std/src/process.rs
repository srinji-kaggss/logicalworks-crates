//! Process-group termination and observation for supervised subprocesses.
//!
//! Stable `std` can kill a direct child (`Child::kill`) but cannot signal a
//! whole process group, so killing a shell leaves orphaned grandchildren.
//! It also cannot observe a child's exit without reaping it, which is the
//! primitive a supervisor needs to keep a group leader's id from being
//! reused under it: a zombie leader still holds its pid, so its group id
//! cannot be reissued until the supervisor has finished signalling. These
//! are the `process` feature's primitives. Unix-only; other targets report
//! [`std::io::ErrorKind::Unsupported`] rather than a fabricated success.

use std::io;

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
        return Err(invalid_pgid());
    }
    let pid = pid_from_raw(pgid)?;
    rustix::process::kill_process_group(pid, rustix::process::Signal::KILL).map_err(errno_to_io)
}

/// Send SIGKILL to every process in group `pgid`.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`] rather than a fabricated success.
#[cfg(all(not(unix), feature = "process"))]
pub fn kill_process_group(pgid: i32) -> io::Result<()> {
    let _ = pgid;
    Err(unsupported("process kill_process_group is Unix-only"))
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
        return Err(invalid_pid());
    }
    let Some(target) = rustix::process::Pid::from_raw(pid) else {
        return Err(invalid_pid());
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

/// Whether child `pid` has exited, leaving it un-reaped.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`] rather than a fabricated success.
#[cfg(all(not(unix), feature = "process"))]
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

#[cfg(all(unix, feature = "process"))]
/// Construct the invalid-process-id error.
fn invalid_pid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "process id must be a positive pid",
    )
}

#[cfg(all(not(unix), feature = "process"))]
/// Construct the unsupported-operation error for non-Unix targets.
fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_pgid_is_rejected() {
        assert!(kill_process_group(0).is_err());
        assert!(kill_process_group(-1).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn nonexistent_group_surfs_os_error() {
        // i32::MAX is not a live group; the OS must refuse, not succeed.
        assert!(kill_process_group(i32::MAX).is_err());
    }

    #[test]
    fn invalid_pid_is_rejected_for_the_exit_observation() {
        assert!(child_has_exited_without_reaping(0).is_err());
        assert!(child_has_exited_without_reaping(-1).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn a_non_child_pid_is_an_error_not_an_observation() {
        // i32::MAX is not our child, so waitid must refuse rather than
        // report an exit; a fabricated `true` would let a caller reap-skip a
        // process that is still running.
        assert!(child_has_exited_without_reaping(i32::MAX).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn a_live_child_is_reported_not_yet_exited() -> Result<(), Box<dyn std::error::Error>> {
        // A real child of this test process, held alive until the probe.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let pid = i32::try_from(child.id())?;
        assert!(
            !child_has_exited_without_reaping(pid)?,
            "live child reported exited"
        );
        child.kill()?;
        let status = child.wait()?;
        assert!(
            !status.success(),
            "the killed probe child must report its signal, not success"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn an_exited_child_is_reported_exited_and_stays_reapable()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut child = std::process::Command::new("true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let pid = i32::try_from(child.id())?;
        // The child may need a moment to actually exit.
        let mut observed_exit = false;
        for _ in 0..100_000 {
            if child_has_exited_without_reaping(pid)? {
                observed_exit = true;
                break;
            }
            std::thread::yield_now();
        }
        assert!(
            observed_exit,
            "an exited child must be observed within the bound"
        );
        // The observation must not have consumed the exit: the reap still
        // returns the real status.
        let status = child.wait()?;
        assert!(status.success(), "`true` must report success after reaping");
        Ok(())
    }
}
