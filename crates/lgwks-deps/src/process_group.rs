//! Non-mutating process-group observation through the dependency storefront.

use std::io;

/// Return whether a positive process-group identifier currently names a group.
///
/// This performs the POSIX `killpg(pgid, 0)` existence/permission check. Signal
/// zero does not deliver a signal. `EPERM` means the group exists but cannot be
/// signalled by this process; `ESRCH` means it is absent. A zero or negative id
/// is rejected to avoid treating zero as the caller's process group.
#[cfg(unix)]
pub fn exists(pgid: i32) -> io::Result<bool> {
    use nix::errno::Errno;
    use nix::sys::signal::killpg;
    use nix::unistd::Pid;

    if pgid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process group id must be positive",
        ));
    }
    let pid = Pid::from_raw(pgid);

    match killpg(pid, None) {
        Ok(()) | Err(Errno::EPERM) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(errno) => Err(errno.into()),
    }
}

/// The process-group probe is unsupported outside Unix.
#[cfg(not(unix))]
pub fn exists(pgid: i32) -> io::Result<bool> {
    let _ = pgid;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process-group existence is Unix-only",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::exists;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    #[test]
    fn observes_a_live_then_reaped_process_group() -> Result<(), Box<dyn std::error::Error>> {
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let group = i32::try_from(child.id())?;
        assert!(exists(group)?, "the live child owns its group");
        child.kill()?;
        child.wait()?;
        assert!(!exists(group)?, "the reaped group is absent");
        Ok(())
    }

    #[test]
    fn rejects_the_callers_group_and_nonpositive_ids() {
        assert_eq!(
            exists(0).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::InvalidInput)
        );
        assert_eq!(
            exists(-1).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::InvalidInput)
        );
    }
}
