//! Process-group termination for supervised subprocesses.
//!
//! Stable `std` can kill a direct child (`Child::kill`) but cannot signal a
//! whole process group, so killing a shell leaves orphaned grandchildren.
//! This is the `process` feature's one primitive: send SIGKILL to every
//! member of a group (usually a group the caller created for a supervised
//! child via `setsid`/`setpgid`). Unix-only; other targets report
//! [`std::io::ErrorKind::Unsupported`] rather than a fabricated success.

use std::io;

/// Send SIGKILL to every process in group `pgid`.
///
/// `pgid` is a process-group id as reported by the OS (a positive integer;
/// typically the leader's pid). An invalid id surfaces the OS error —
/// never silent success. This kills unconditionally (SIGKILL cannot be
/// caught); callers that need graceful shutdown must signal SIGTERM
/// themselves before falling back here.
#[cfg(all(unix, feature = "process"))]
pub fn kill_process_group(pgid: i32) -> io::Result<()> {
    // Reject before touching rustix: 0 names the CALLER's group (SIGKILL
    // would hit us) and negatives panic inside Pid::from_raw in debug.
    if pgid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process group id must be a positive pid",
        ));
    }
    let pid = rustix::process::Pid::from_raw(pgid).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "process group id must be a positive pid",
        )
    })?;
    rustix::process::kill_process_group(pid, rustix::process::Signal::KILL)
        .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))
}

/// Send SIGKILL to every process in group `pgid`.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`] rather than a fabricated success.
#[cfg(all(not(unix), feature = "process"))]
pub fn kill_process_group(pgid: i32) -> io::Result<()> {
    let _ = pgid;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process kill_process_group is Unix-only",
    ))
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
}
