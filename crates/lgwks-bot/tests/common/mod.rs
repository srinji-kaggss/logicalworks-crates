//! Test scaffolding shared by more than one integration binary.
//!
//! Every crate's integration tests are separate binaries, so a helper written
//! in one of them is invisible to the rest — which is how two crash
//! harnesses ended up carrying byte-identical copies of a scratch-directory
//! allocator and a temporary-directory guard. Two copies drift: the second one
//! is fixed only when a test fails, and never while both pass.
//!
//! This is the one copy. A helper belongs here when two binaries need it
//! *identically*, not merely similarly.

#![allow(dead_code, reason = "each binary uses a different subset")]

use std::path::PathBuf;

use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectKey, EnvironmentEpoch, EnvironmentId, FlowRevision,
    RunId,
};

/// The fixed identity every crash harness here journals under: one run, one
/// action, one environment, one flow revision.
///
/// Three binaries each needed an [`EffectKey`] and each wrote the same ten
/// lines. The identity is part of what makes the observations comparable — two
/// harnesses that disagreed about the run would fold different worlds — so it is
/// defined once, here, rather than per file.
pub const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
pub const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
pub const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
pub const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

/// The key for attempt `attempt` under the shared identity.
///
/// `digest` is the attempt's content identity; a different digest is a
/// different attempt with its own ladder, which is exactly the distinction the
/// recovery rows turn on.
pub fn key(attempt: &str, digest: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
    Ok(EffectKey::new(
        RunId::from_hex(RUN)?,
        ActionId::from_hex(ACTION)?,
        AttemptId::from_decimal(attempt)?,
        FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
        ActionDigest::from_tagged("blake3_256", digest)?,
        EnvironmentId::from_hex(ENV)?,
        EnvironmentEpoch::from_decimal("1")?,
    ))
}

/// Gives concurrent test runs distinct scratch names.
///
/// Nanos plus a counter, not a process id: the OS reuses both pids and
/// threads, and a reused id must never make two runs share a file.
static SCRATCH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A unique scratch directory for one test.
///
/// The path comes from `std::env::temp_dir`, so no host-specific absolute path
/// is baked into a fixture and the directory is wherever the host keeps
/// temporaries.
pub fn scratch(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    use std::sync::atomic::Ordering;
    let dir = std::env::temp_dir().join(format!(
        "lgwks-test-{name}-{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
        SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Removes a test's scratch directory when the test ends, however it ends.
///
/// The guard is best effort: a scratch directory the system refuses to remove
/// is litter, not a failed observation, so the error is dropped rather than
/// allowed to mask the test's own verdict.
pub struct TempGuard(pub PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.0.is_dir() {
            drop(std::fs::remove_dir_all(&self.0));
        } else {
            drop(std::fs::remove_file(&self.0));
        }
    }
}

/// Kills a probe child and reaps it, however the test ends.
///
/// `take` hands the child to the kill harness so no path kills twice; the drop
/// is the backstop for a test that returns early, or panics, between the spawn
/// and the kill. A harness that forgets to kill must not leave a live child
/// behind, and this is what makes that true without every test remembering.
pub struct ProbeGuard(pub Option<std::process::Child>);

impl ProbeGuard {
    /// Hand the child to the kill harness.
    pub fn take(&mut self) -> Option<std::process::Child> {
        self.0.take()
    }
}

impl ProbeGuard {
    /// Wait for `marker`, then `SIGKILL` the child and reap it.
    ///
    /// `Child::kill` sends `SIGKILL` on Unix: no cleanup, no destructors, no
    /// flushing. Whatever the child wrote that is not on the disk is gone, and
    /// whatever claimed to be durable had better be there. The wait is bounded
    /// so a child that never reaches its marker cannot leave a stray process.
    ///
    /// Unix-only, like every `SIGKILL` claim; a caller on another platform
    /// gets the platform's own kill, which is not this observation.
    #[cfg(unix)]
    pub fn kill_after_marker(
        &mut self,
        marker: &std::path::Path,
        test_name: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::process::ExitStatusExt as _;

        let Some(mut child) = self.take() else {
            return Err(format!("the probe child for {test_name} was gone before the kill").into());
        };
        for _ in 0..2_000 {
            if marker.exists() {
                child.kill()?;
                let status = child.wait()?;
                assert_eq!(
                    status.signal(),
                    Some(9),
                    "the probe must have been killed, not exited: {status}"
                );
                return Ok(());
            }
            if let Some(status) = child.try_wait()? {
                return Err(
                    format!("the probe child exited on its own before the kill: {status}").into(),
                );
            }
            poll_pause();
        }
        child.kill()?;
        child.wait()?;
        Err(format!(
            "the probe child {test_name} never reached its marker; the observation has no kill \
             to test"
        )
        .into())
    }
}

/// A bounded pause between marker polls.
///
/// The workspace's ban on `std::thread::sleep` exists because blocking an
/// executor thread stalls every task on it. A crash harness has no executor:
/// the child parks while the parent decides when it dies, and that wait is the
/// observation's subject rather than its scaffolding.
#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "the kill harness is a plain process with no async runtime and no reactor to \
              stall; the parked child and the marker poll are the observation's shape"
)]
fn poll_pause() {
    std::thread::sleep(std::time::Duration::from_millis(5));
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            drop(child.kill());
            drop(child.wait());
        }
    }
}
