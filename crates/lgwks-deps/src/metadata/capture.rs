//! The bounded `cargo` subprocess: capture files, the deadline, the byte budgets and the cleanup obligation.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::MetadataError;

/// Elapsed-deadline budget for the Cargo subprocess: `cargo metadata` on
/// this workspace answers in about a second, so a healthy run never notices
/// the 120s collection ceiling. The ceiling bounds collection polling; OS
/// termination and reap failures are reported separately (issue #159).
pub(super) const METADATA_TIMEOUT: Duration = Duration::from_secs(120);
/// Per-stream retained-byte budget for the Cargo subprocess. The 5ms polling
/// interval makes disk growth between samples possible, so this is not a hard
/// on-disk quota (issue #159 M3).
pub(super) const METADATA_STREAM_CAP: usize = 8 * 1024 * 1024;
/// Exclusive-create attempts per capture file: each attempt draws a fresh
/// distinguisher, so this bound is never reached by chance, only by a
/// broken clock or a hostile temp dir, both of which refuse (issue #143 R14).
const CAPTURE_ATTEMPTS: usize = 16;

/// A reaped child's exit status with its bounded output.
pub(super) struct BoundedOutput {
    /// What the child exited with; a non-zero status is Cargo's refusal.
    pub(super) status: std::process::ExitStatus,
    /// At most the per-stream budget of stdout bytes.
    pub(super) stdout: Vec<u8>,
    /// At most the per-stream budget of stderr bytes.
    pub(super) stderr: Vec<u8>,
    /// A capture-removal failure after complete output, reported beside it.
    pub(super) unresolved: Option<Unresolved>,
}

/// Capture removals that failed after a complete collection, and the owner
/// that can retry them.
#[derive(Debug)]
pub(super) struct Unresolved {
    /// Every removal failure, in the order of the obligation's report.
    pub(super) captures: Vec<std::io::Error>,
    /// The owner of the paths that could not be removed.
    pub(super) obligation: Box<CleanupObligation>,
}

impl Unresolved {
    /// The cleanup failure as a refusal, with `cause` as its primary failure
    /// when collection was refused afterwards.
    pub(super) fn into_error(self, cause: Option<MetadataError>) -> MetadataError {
        MetadataError::ProcessCleanup {
            cause: cause.map(Box::new),
            process: None,
            captures: self.captures,
            obligation: self.obligation,
        }
    }
}

/// Owns capture pathnames from the instant a capture file is created through
/// every later fallible collection step.
#[derive(Debug)]
pub(super) struct CaptureFiles {
    /// Paths whose names this call created exclusively.
    pub(super) paths: Vec<PathBuf>,
}

/// Calls the operating system unless one test has armed a single-use fault.
fn remove_capture(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if let Some(error) = super::faults::take_fault(super::faults::FaultPoint::Unlink) {
        let refusal = Err(error);
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "remove_capture: returning an error to the caller");
        return refusal;
    }
    std::fs::remove_file(path)
}

/// Observes the direct child without treating an observation error as absence.
fn observe_child(
    child: &mut std::process::Child,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    #[cfg(test)]
    if let Some(error) = super::faults::take_fault(super::faults::FaultPoint::WaitObservation) {
        let refusal = Err(error);
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "observe_child: returning an error to the caller");
        return refusal;
    }
    child.try_wait()
}

/// Requests termination and preserves an OS refusal for the cleanup owner.
fn terminate_child(child: &mut std::process::Child) -> std::io::Result<()> {
    #[cfg(test)]
    if let Some(error) = super::faults::take_fault(super::faults::FaultPoint::Kill) {
        let refusal = Err(error);
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "terminate_child: returning an error to the caller");
        return refusal;
    }
    child.kill()
}

/// Waits for a direct child; an error leaves it in the retryable owner.
fn reap_child(child: &mut std::process::Child) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(test)]
    if let Some(error) = super::faults::take_fault(super::faults::FaultPoint::Reap) {
        let refusal = Err(error);
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "reap_child: returning an error to the caller");
        return refusal;
    }
    child.wait()
}

impl CaptureFiles {
    /// Starts an empty cleanup owner before acquiring the first file.
    pub(super) fn new() -> Self {
        Self { paths: Vec::new() }
    }

    /// Records a newly acquired path before another operation can fail.
    pub(super) fn own(&mut self, path: PathBuf) {
        #[cfg(test)]
        super::faults::record_capture(path.clone());
        self.paths.push(path);
    }

    /// Removes every owned path and returns every failure except confirmed
    /// absence. Failed removals remain visible to the caller.
    pub(super) fn cleanup(&mut self) -> Vec<std::io::Error> {
        let mut failures = Vec::new();
        let mut pending = Vec::new();
        for path in self.paths.drain(..) {
            match remove_capture(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    failures.push(error);
                    pending.push(path);
                }
            }
        }
        self.paths = pending;
        failures
    }
}

impl Drop for CaptureFiles {
    /// Retries cleanup on early return; this fallback never reports success.
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

/// Retains ownership of a direct child or capture whose cleanup was not
/// confirmed. Keep this value and retry until it reports `true`.
#[must_use = "dropping an unresolved cleanup obligation abandons further observation"]
#[derive(Debug)]
pub struct CleanupObligation {
    /// The child whose termination or reap remains unresolved.
    child: Option<std::process::Child>,
    /// Capture paths whose removal remains unresolved.
    pub(super) captures: CaptureFiles,
    /// What failed when the obligation was created, fixed so the refusal's
    /// text does not change as retries make progress.
    pub(super) report: CleanupReport,
}

/// The facts a cleanup refusal names: which step failed, on which process, and
/// which capture paths could not be removed.
#[derive(Debug)]
pub(super) struct CleanupReport {
    /// The process step that failed, if one did.
    pub(super) step: Option<CleanupStep>,
    /// The direct child's process id, while the obligation still owns it.
    pub(super) pid: Option<u32>,
    /// Capture paths whose removal failed, in the order of the retained errors.
    pub(super) capture_paths: Vec<PathBuf>,
}

/// A process-cleanup step that can fail and leave the child owned.
#[derive(Debug, Clone, Copy)]
pub(super) enum CleanupStep {
    /// Sending the kill request.
    Kill,
    /// Waiting for the exited child to be reaped.
    Reap,
}

impl CleanupStep {
    /// The verb a refusal uses for this step.
    pub(super) fn verb(self) -> &'static str {
        match self {
            Self::Kill => "kill",
            Self::Reap => "reap",
        }
    }
}

impl CleanupObligation {
    /// Takes ownership of an unresolved child and capture set, recording what
    /// failed at this moment for the refusal's text.
    pub(super) fn new(
        step: Option<CleanupStep>,
        child: Option<std::process::Child>,
        captures: &mut CaptureFiles,
    ) -> Self {
        let paths = std::mem::take(&mut captures.paths);
        Self {
            report: CleanupReport {
                step,
                pid: child.as_ref().map(std::process::Child::id),
                capture_paths: paths.clone(),
            },
            child,
            captures: CaptureFiles { paths },
        }
    }
}

impl CleanupObligation {
    /// A caller holding a [`MetadataError::ProcessCleanup`] calls this on its `obligation`
    /// until it answers `true`; the unit tests `cleanup_obligation_confirms_an_already_exited_child`
    /// and `injected_collection_fault_matrix_keeps_cleanup_owned_and_truthful` run that retry.
    pub fn retry_cleanup(&mut self) -> Result<bool, std::io::Error> {
        if let Some(child) = self.child.as_mut() {
            match terminate_child(child) {
                Ok(()) => {}
                Err(kill_error) => match child.try_wait() {
                    Ok(Some(_)) => self.child = None,
                    Ok(None) => {
                        let refusal = Err(kill_error);
                        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "retry_cleanup: returning an error to the caller");
                        return refusal;
                    }
                    Err(observation_error) => {
                        let refusal = Err(observation_error);
                        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "retry_cleanup: returning an error to the caller");
                        return refusal;
                    }
                },
            }
            if let Some(child) = self.child.as_mut() {
                match observe_child(child)? {
                    Some(_) => self.child = None,
                    None => return Ok(false),
                }
            }
        }
        if let Some(error) = self.captures.cleanup().into_iter().next() {
            let refusal = Err(error);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "retry_cleanup: returning an error to the caller");
            return refusal;
        }
        Ok(self.captures.paths.is_empty())
    }
}

/// How many poll quanta a dropped obligation waits for its child to exit.
///
/// Twenty 5 ms quanta: long enough for a killed child to be reaped on a loaded
/// host, short enough that dropping an error never stalls the caller for more
/// than a tenth of a second.
pub(super) const DROP_CLEANUP_QUANTA: usize = 20;

/// A dropped obligation makes a last, bounded attempt to kill and reap.
///
/// The shipped gate reports a `ProcessCleanup` refusal as text and drops the
/// error, so without this the Cargo child it names was never killed or reaped
/// by anyone. A killed child is not reaped the instant the signal is sent, so
/// one `try_wait` would usually leave a zombie; this repeats the same `kill`
/// plus `try_wait` a retry makes for at most twenty poll quanta and then gives
/// up, because a destructor must not block without bound. Capture files have
/// their own `Drop`.
impl Drop for CleanupObligation {
    fn drop(&mut self) {
        for _ in 0..DROP_CLEANUP_QUANTA {
            if matches!(self.retry_cleanup(), Ok(true)) {
                return;
            }
            poll_quantum();
        }
    }
}

/// The largest capture `Take` can be asked to count, and the ceiling a declared
/// cap above it becomes.
///
/// `Take` counts in `u64` and a cap is a `usize`; a cap past `u64::MAX` is one no
/// platform can measure, so the ceiling is the only bound that is true of it.
/// The bound is named because two call sites needed it, and a ceiling written
/// twice is two ceilings.
const MAX_CAPTURE_BYTES: u64 = u64::MAX;

/// The byte ceiling `read_capped` enforces, in the width `Take` counts in.
fn capture_limit(cap: usize) -> u64 {
    match u64::try_from(cap) {
        Ok(limit) => limit,
        Err(_) => MAX_CAPTURE_BYTES,
    }
}

/// One deadline-poll quantum.
///
/// A timed condvar wait rather than `sleep`: this crate is synchronous, so there
/// is no reactor to block, and a condvar wait is a bounded wait on an event
/// source. The condvar and its mutex are local to the call and nothing notifies
/// them, so the wait always ends by its timeout; the 5 ms quantum bounds
/// deadline overshoot and the poll loop is the only waiter, so it always stops.
pub(super) fn poll_quantum() {
    let quiet = std::sync::Mutex::new(());
    let wake = std::sync::Condvar::new();
    let held = match quiet.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };
    match wake.wait_timeout(held, POLL_QUANTUM) {
        Ok(woken) => drop(woken),
        Err(poisoned) => drop(poisoned.into_inner()),
    }
}

/// The length of one deadline-poll quantum.
const POLL_QUANTUM: Duration = Duration::from_millis(5);

/// Runs one subprocess to completion under a deadline and output budgets.
///
/// The child writes to two capture files, never to pipes: a flood cannot
/// block the child on a full pipe, and no reader thread can be held open by
/// a grandchild that inherited a descriptor. Each poll quantum stats both
/// files, and the first one past budget requests direct-child termination
/// before returning [`MetadataError::OutputTooLarge`]; the deadline does the
/// same with [`MetadataError::Timeout`]. Termination, reap and capture-removal
/// failures are retained in [`MetadataError::ProcessCleanup`]. A refusal
/// carries no partial graph: the gate reports the failure rather than
/// decoding a prefix of it.
///
/// The kill targets the direct child only. A descendant may keep an inherited
/// capture descriptor and continue writing after the direct child's exit; the
/// sampled retained-byte cap does not bound that writer's disk use or reclaim
/// its open file immediately. The supported profile is trusted Cargo and its
/// direct child, not hostile descendant containment.
pub(super) fn run_bounded(
    program: &str,
    args: &[OsString],
    dir: &Path,
    timeout: Duration,
    stream_cap: usize,
) -> Result<BoundedOutput, MetadataError> {
    use std::fs::File;
    use std::io::Read as _;
    use std::process::Stdio;

    /// One capture file: created empty and exclusive, unlinked on every
    /// return path. Creation uses `create_new`, so even a distinguisher
    /// collision is a retried name, never another call's file truncated.
    fn capture(name: &str) -> Result<(PathBuf, File), MetadataError> {
        use std::io::ErrorKind;
        #[cfg(test)]
        {
            let fault = if name == "stdout" {
                super::faults::FaultPoint::CaptureStdout
            } else {
                super::faults::FaultPoint::CaptureStderr
            };
            if let Some(error) = super::faults::take_fault(fault) {
                let refusal = Err(MetadataError::Spawn(error));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "capture: returning an error to the caller");
                return refusal;
            }
        }
        // Bounded: each attempt draws a fresh distinguisher, so exhaustion
        // is a refusal shape, not a spin.
        for _ in 0..CAPTURE_ATTEMPTS {
            let path = std::env::temp_dir().join(format!(
                "lgwks-deps-{name}-{distinguisher}.capture",
                distinguisher = distinguisher()?,
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    let refusal = Err(MetadataError::Spawn(error));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "capture: returning an error to the caller");
                    return refusal;
                }
            }
        }
        Err(MetadataError::Spawn(std::io::Error::from(
            ErrorKind::AlreadyExists,
        )))
    }

    /// Names one capture file: 128 bits of OS entropy (`lgwks_std::random`,
    /// the one source INV-RANDOM-ONE-SOURCE allows). A process id or a clock
    /// would be reused by the OS and could name another call's file. An
    /// entropy refusal names no file and starts no child; the call ends here.
    #[cfg(not(target_family = "wasm"))]
    fn distinguisher() -> Result<String, MetadataError> {
        lgwks_std::random::bytes::<16>()
            .map(lgwks_std::hex::encode)
            .map_err(MetadataError::Entropy)
    }

    /// wasm has no entropy source: `lgwks_std::random` refuses the target
    /// with a `compile_error!`, and the WASI boundary job builds this crate
    /// for `wasm32-wasip1`. Nanos plus a monotone sequence name the file,
    /// and `create_new` above is what makes that safe: a repeated name is
    /// retried, never opened. Spawning cannot succeed on this target anyway,
    /// so these files only ever live until the `Spawn` refusal unlinks them.
    #[cfg(target_family = "wasm")]
    fn distinguisher() -> Result<String, MetadataError> {
        static CAPTURE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        // A clock that reads before the Unix epoch has no nanos to offer, and
        // flooring it to the epoch would name every capture of this process the
        // same instant. The sequence alone cannot separate two runs, so the
        // refusal is the honest answer: a name this process cannot distinguish is
        // not a distinguishable name.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|before_epoch| {
                MetadataError::Spawn(std::io::Error::other(format!(
                    "the wall clock reads before the Unix epoch: {before_epoch}"
                )))
            })?
            .as_nanos();
        let seq = CAPTURE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(format!("{nanos}-{seq}"))
    }

    /// Read at most one byte past budget: anything longer is an overflow,
    /// and the retained bytes are dropped with the Vec, never decoded.
    fn read_capped(
        path: &Path,
        stream_cap: usize,
        _stream: &'static str,
    ) -> Result<(Vec<u8>, bool), MetadataError> {
        #[cfg(test)]
        {
            let fault = if _stream == "stdout" {
                super::faults::FaultPoint::ReadStdout
            } else {
                super::faults::FaultPoint::ReadStderr
            };
            if let Some(error) = super::faults::take_fault(fault) {
                let refusal = Err(MetadataError::Spawn(error));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read_capped: returning an error to the caller");
                return refusal;
            }
        }
        let limit = capture_limit(stream_cap);
        let file = File::open(path).map_err(MetadataError::Spawn)?;
        let mut kept = Vec::new();
        file.take(limit.saturating_add(1))
            .read_to_end(&mut kept)
            .map_err(MetadataError::Spawn)?;
        let overflow = kept.len() > stream_cap;
        Ok((kept, overflow))
    }

    /// Keeps the original failure distinct from any cleanup failure.
    fn cleanup_error(
        cause: Option<MetadataError>,
        process: Option<(CleanupStep, std::io::Error)>,
        child: Option<std::process::Child>,
        captures: &mut CaptureFiles,
    ) -> MetadataError {
        let capture_errors = captures.cleanup();
        if process.is_none()
            && capture_errors.is_empty()
            && let Some(cause) = cause
        {
            return cause;
        }
        let (step, process) = match process {
            Some((step, error)) => (Some(step), Some(error)),
            None => (None, None),
        };
        MetadataError::ProcessCleanup {
            cause: cause.map(Box::new),
            process,
            captures: capture_errors,
            obligation: Box::new(CleanupObligation::new(step, child, captures)),
        }
    }

    let mut captures = CaptureFiles::new();
    let (stdout_path, stdout_file) = match capture("stdout") {
        Ok(capture) => capture,
        Err(error) => {
            let refusal = Err(cleanup_error(Some(error), None, None, &mut captures));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "cleanup_error: returning an error to the caller");
            return refusal;
        }
    };
    captures.own(stdout_path.clone());
    let (stderr_path, stderr_file) = match capture("stderr") {
        Ok(capture) => capture,
        Err(error) => {
            let refusal = Err(cleanup_error(Some(error), None, None, &mut captures));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "cleanup_error: returning an error to the caller");
            return refusal;
        }
    };
    captures.own(stderr_path.clone());
    #[cfg(test)]
    let spawn_fault = super::faults::take_fault(super::faults::FaultPoint::Spawn);
    #[cfg(not(test))]
    let spawn_fault: Option<std::io::Error> = None;
    let spawned = match spawn_fault {
        Some(error) => Err(error),
        None => Command::new(program)
            .args(args)
            .current_dir(dir)
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .spawn(),
    };
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            let refusal = Err(cleanup_error(
                Some(MetadataError::Spawn(error)),
                None,
                None,
                &mut captures,
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "cleanup_error: returning an error to the caller");
            return refusal;
        }
    };
    // A deadline the monotonic clock cannot represent is not a deadline: the
    // clock's range is finite and a `timeout` past its end never arrives, so the
    // byte budget is what bounds the child. Collapsing it to `now` instead would
    // refuse every child immediately over a timeout of a few centuries.
    let deadline = std::time::Instant::now().checked_add(timeout);
    // The only waiter, and it always stops: each quantum either observes
    // the exit, fires the deadline, or finds a capture file past budget.
    // File sizes only grow while the child lives, so a stat past budget
    // stands against every later one.
    let outcome = loop {
        match observe_child(&mut child) {
            Err(error) => break Err(MetadataError::Spawn(error)),
            Ok(Some(status)) => break Ok(status),
            Ok(None) if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) => {
                break Err(MetadataError::Timeout { after: timeout });
            }
            Ok(None) => {
                let limit = capture_limit(stream_cap);
                #[cfg(test)]
                let stdout_stat_fault =
                    super::faults::take_fault(super::faults::FaultPoint::StatStdout);
                #[cfg(not(test))]
                let stdout_stat_fault: Option<std::io::Error> = None;
                if let Some(error) = stdout_stat_fault {
                    break Err(MetadataError::Spawn(error));
                }
                let stdout_size = match std::fs::metadata(&stdout_path) {
                    Ok(size) => size,
                    Err(error) => break Err(MetadataError::Spawn(error)),
                };
                if stdout_size.len() > limit {
                    break Err(MetadataError::OutputTooLarge {
                        stream: "stdout",
                        limit: stream_cap,
                    });
                }
                #[cfg(test)]
                let stderr_stat_fault =
                    super::faults::take_fault(super::faults::FaultPoint::StatStderr);
                #[cfg(not(test))]
                let stderr_stat_fault: Option<std::io::Error> = None;
                if let Some(error) = stderr_stat_fault {
                    break Err(MetadataError::Spawn(error));
                }
                let stderr_size = match std::fs::metadata(&stderr_path) {
                    Ok(size) => size,
                    Err(error) => break Err(MetadataError::Spawn(error)),
                };
                if stderr_size.len() > limit {
                    break Err(MetadataError::OutputTooLarge {
                        stream: "stderr",
                        limit: stream_cap,
                    });
                }
                poll_quantum();
            }
        }
    };
    let status = match outcome {
        Err(cause) => {
            let process_error = match terminate_child(&mut child) {
                Ok(()) => reap_child(&mut child)
                    .err()
                    .map(|error| (CleanupStep::Reap, error)),
                Err(kill_error) => match child.try_wait() {
                    Ok(Some(_)) => None,
                    Ok(None) | Err(_) => Some((CleanupStep::Kill, kill_error)),
                },
            };
            return match process_error {
                Some(error) => Err(cleanup_error(
                    Some(cause),
                    Some(error),
                    Some(child),
                    &mut captures,
                )),
                None => Err(cleanup_error(Some(cause), None, None, &mut captures)),
            };
        }
        Ok(status) => match reap_child(&mut child) {
            Ok(_) => status,
            Err(error) => {
                let refusal = Err(cleanup_error(
                    None,
                    Some((CleanupStep::Reap, error)),
                    Some(child),
                    &mut captures,
                ));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "cleanup_error: returning an error to the caller");
                return refusal;
            }
        },
    };
    let (stdout, stdout_overflow) = match read_capped(&stdout_path, stream_cap, "stdout") {
        Ok(output) => output,
        Err(error) => {
            let refusal = Err(cleanup_error(Some(error), None, None, &mut captures));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "cleanup_error: returning an error to the caller");
            return refusal;
        }
    };
    let (stderr, stderr_overflow) = match read_capped(&stderr_path, stream_cap, "stderr") {
        Ok(output) => output,
        Err(error) => {
            let refusal = Err(cleanup_error(Some(error), None, None, &mut captures));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "cleanup_error: returning an error to the caller");
            return refusal;
        }
    };
    let overflow = if stdout_overflow {
        Some(MetadataError::OutputTooLarge {
            stream: "stdout",
            limit: stream_cap,
        })
    } else if stderr_overflow {
        Some(MetadataError::OutputTooLarge {
            stream: "stderr",
            limit: stream_cap,
        })
    } else {
        None
    };
    if let Some(cause) = overflow {
        let refusal = Err(cleanup_error(Some(cause), None, None, &mut captures));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "cleanup_error: returning an error to the caller");
        return refusal;
    }
    // Both streams are read in full and the child is reaped, so the output is
    // complete. A capture that cannot be removed now is reported beside it,
    // not in place of it: the graph is valid, and the leftover path is owned
    // by the obligation for a retry.
    let cleanup_errors = captures.cleanup();
    let unresolved = (!cleanup_errors.is_empty()).then(|| Unresolved {
        captures: cleanup_errors,
        obligation: Box::new(CleanupObligation::new(None, None, &mut captures)),
    });
    Ok(BoundedOutput {
        status,
        stdout,
        stderr,
        unresolved,
    })
}
