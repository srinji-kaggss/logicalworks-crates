//! The single-use faults the collection tests inject into the subprocess, and the checks they share.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;

use super::capture::{CleanupObligation, DROP_CLEANUP_QUANTA, poll_quantum};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FaultPoint {
    CaptureStdout,
    CaptureStderr,
    Spawn,
    WaitObservation,
    StatStdout,
    StatStderr,
    ReadStdout,
    ReadStderr,
    Kill,
    Reap,
    Unlink,
}

thread_local! {
    pub(super) static FAULTS: RefCell<VecDeque<FaultPoint>> = const { RefCell::new(VecDeque::new()) };
    static OWNED_CAPTURES: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn take_fault(point: FaultPoint) -> Option<std::io::Error> {
    FAULTS.with(|faults| {
        if faults.borrow().front() == Some(&point) {
            faults.borrow_mut().pop_front();
            Some(std::io::Error::other(format!(
                "injected metadata fault at {point:?}"
            )))
        } else {
            None
        }
    })
}

pub(super) fn arm_faults(points: &[FaultPoint]) {
    FAULTS.with(|faults| *faults.borrow_mut() = points.iter().copied().collect());
    OWNED_CAPTURES.with(|paths| paths.borrow_mut().clear());
}

pub(super) fn record_capture(path: PathBuf) {
    OWNED_CAPTURES.with(|paths| paths.borrow_mut().push(path));
}

pub(super) fn captured_paths() -> Vec<PathBuf> {
    OWNED_CAPTURES.with(|paths| std::mem::take(&mut *paths.borrow_mut()))
}

/// Retries cleanup for the same bounded quanta `Drop` allows. A killed
/// child is not reaped the instant the signal lands, so one
/// `retry_cleanup` legitimately reports `false` on a loaded host.
pub(super) fn retry_until_resolved(obligation: &mut CleanupObligation) -> std::io::Result<bool> {
    for _ in 0..DROP_CLEANUP_QUANTA {
        if obligation.retry_cleanup()? {
            return Ok(true);
        }
        poll_quantum();
    }
    obligation.retry_cleanup()
}

pub(super) fn assert_paths_removed(paths: &[PathBuf]) -> TestResult {
    for path in paths {
        assert_eq!(
            std::fs::metadata(path).err().map(|error| error.kind()),
            Some(std::io::ErrorKind::NotFound),
            "capture path should be gone: {}",
            path.display()
        );
    }
    Ok(())
}

/// Test bodies propagate with `?` rather than panicking: `unwrap` is
/// forbidden workspace-wide, and a failing edge extraction should surface
/// as the error it is, not as a panic with no variant attached.
pub(super) type TestResult = Result<(), Box<dyn std::error::Error>>;
