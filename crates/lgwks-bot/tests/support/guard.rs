//! The child-process watchdog for test bodies whose failure mode is a hang.
//!
//! A body that may never return cannot report on itself, and libtest has no
//! per-test deadline, so [`guarded`] re-executes the test binary as a child
//! filtered to one test and waits on that child with a deadline. The wait is
//! on the child's own exit, read as end-of-file on its piped stdout, which the
//! operating system delivers when the child exits or is killed: there is no
//! poll interval to tune and no sleep standing in for an event.

use std::io::Write;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::Duration;

type GuardResult = Result<(), Box<dyn std::error::Error>>;

/// The environment variable that tells a re-executed child it is the guarded
/// run rather than the parent.
const GUARD_ENV: &str = "LGWKS_BOT_ECS_TICK_CHILD";

/// How long the parent waits for the child before killing it.
///
/// The guarded bodies finish in milliseconds; the margin is for a loaded
/// machine, and every second of it is spent only when something is already
/// wrong.
const GUARD_DEADLINE: Duration = Duration::from_secs(12);

/// Run `body` in a child process, and fail the parent if that child does not
/// finish within [`GUARD_DEADLINE`].
///
/// The child is this same test binary, re-executed with [`GUARD_ENV`] set and
/// filtered to `test_name`, so it runs exactly one body and does not recurse.
/// A body that returns `Ok` leaves a marker file behind, which the parent
/// checks: a successful exit with no marker means the filter matched nothing
/// and the test never ran, which would otherwise be indistinguishable from a
/// pass.
///
/// The child's stdout is piped and copied through by a reader thread, which
/// reaches end-of-file only when the child's end of the pipe closes — at its
/// exit, or at its kill. That end-of-file is sent on a one-slot channel, and
/// the parent's bounded `recv_timeout` on it is the whole deadline. The reader
/// runs inside a thread scope, so it is joined before this returns on every
/// path, the kill path included.
pub(crate) fn guarded(test_name: &'static str, body: fn() -> GuardResult) -> GuardResult {
    if std::env::var_os(GUARD_ENV).is_some() {
        let outcome = body();
        if outcome.is_ok() {
            std::fs::write(marker_path(test_name), test_name)?;
        }
        return outcome;
    }

    let marker = marker_path(test_name);
    discard_marker(&marker)?;
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args([test_name, "--exact", "--nocapture"])
        .env(GUARD_ENV, test_name)
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let Some(mut output) = child.stdout.take() else {
        // A piped stdout is always present; a child without one cannot be
        // watched, so it is killed rather than waited on blind.
        let killed = child.kill().and_then(|()| child.wait());
        return Err(
            format!("the guarded child for `{test_name}` has no stdout ({killed:?})").into(),
        );
    };

    // One slot: the reader sends exactly once, so its send never blocks.
    let (closed, exited) = sync_channel::<()>(1);
    std::thread::scope(|scope| {
        let reader = scope.spawn(move || {
            let copied = std::io::copy(&mut output, &mut std::io::stdout().lock());
            // The receiver outlives this scope, so the send has a listener
            // and a free slot; a refusal is reported, not assumed away.
            let signalled = closed.send(()).is_ok();
            (copied, signalled)
        });
        let verdict = match exited.recv_timeout(GUARD_DEADLINE) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => match child.wait() {
                Ok(status) => exit_verdict(test_name, &marker, status),
                Err(error) => Err(error.into()),
            },
            Err(RecvTimeoutError::Timeout) => match child.kill().and_then(|()| child.wait()) {
                Ok(_killed) => Err(format!(
                    "the guarded child for `{test_name}` did not finish within \
                     {GUARD_DEADLINE:?} and was killed. A tick that never returns is the \
                     defect this bounds."
                )
                .into()),
                Err(error) => Err(format!(
                    "the guarded child for `{test_name}` timed out and could not be killed: \
                     {error}"
                )
                .into()),
            },
        };
        // The child has exited or been killed, so its end of the pipe is
        // closed and the reader has reached end-of-file by the time it is
        // joined.
        match reader.join() {
            Ok((Ok(_bytes), true)) => {}
            Ok((Ok(_bytes), false)) => {
                return Err("the watchdog stopped listening before the child exited".into());
            }
            Ok((Err(error), _)) => return Err(error.into()),
            Err(_) => return Err("the guarded child's output reader panicked".into()),
        }
        std::io::stdout().flush()?;
        verdict
    })
}

/// What a child that exited on its own says about its body.
fn exit_verdict(
    test_name: &str,
    marker: &std::path::Path,
    status: std::process::ExitStatus,
) -> GuardResult {
    if !status.success() {
        return refuse(format!(
            "the guarded child for `{test_name}` failed on its own: {status}"
        ));
    }
    let recorded = match std::fs::read_to_string(marker) {
        Ok(recorded) => recorded,
        Err(error) => {
            return refuse(format!(
                "the guarded child for `{test_name}` exited successfully but never ran the body \
                 ({error}); the child's filter matched no test"
            ));
        }
    };
    if recorded != test_name {
        return refuse(format!(
            "the guarded child for `{test_name}` wrote a marker for `{recorded}`"
        ));
    }
    discard_marker(marker)
}

/// A refusal the watchdog reports, traced on its way to the caller.
fn refuse(reason: String) -> GuardResult {
    let refusal = Err(reason.into());
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "guarded: returning an error to the caller");
    refusal
}

/// Where the child records that it reached the end of its body.
fn marker_path(test_name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("lgwks-bot-ecs-tick-{test_name}.marker"))
}

/// Remove a marker, tolerating its absence.
///
/// Absence is the expected state before a run, so a missing file is not an
/// error; anything else is, because a marker that cannot be cleared would let a
/// previous run's success stand in for this one's.
fn discard_marker(marker: &std::path::Path) -> GuardResult {
    match std::fs::remove_file(marker) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
