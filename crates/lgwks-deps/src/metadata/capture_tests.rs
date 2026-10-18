//! The bounded subprocess's tests: the deadline, the byte budgets, owned captures and the injected-fault
//! matrix (INV-DEP-8).

use std::ffi::OsString;
use std::path::Path;
#[cfg(unix)]
use std::process::Command;
use std::time::Duration;

use super::MetadataError;
use super::capture::{
    BoundedOutput, CaptureFiles, CleanupObligation, CleanupStep, Unresolved, run_bounded,
};
use super::collect::Collected;
use super::faults::{
    FAULTS, FaultPoint, TestResult, arm_faults, assert_paths_removed, captured_paths,
    retry_until_resolved,
};

/// A controllable stand-in for the `cargo` subprocess: sleep, flood, or
/// emit, with no network, no lockfile, and no toolchain beyond the
/// platform shell. `cfg`, not runtime detection: the command must exist
/// where the test compiles.
#[cfg(unix)]
fn sleeper() -> (&'static str, Vec<OsString>) {
    // Background-plus-wait, not a bare sleep: the shell forks a
    // grandchild that inherits the capture files, which is the shape
    // that held pipes open and defeated a pipe-based design on hosted
    // CI. Killing the direct child must still end the call at the
    // deadline even with the grandchild's descriptors open.
    (
        "sh",
        vec![OsString::from("-c"), OsString::from("sleep 10 & wait")],
    )
}

#[cfg(not(unix))]
fn sleeper() -> (&'static str, Vec<OsString>) {
    (
        "powershell",
        vec![
            OsString::from("-NoProfile"),
            OsString::from("-Command"),
            OsString::from("Start-Sleep -Seconds 10"),
        ],
    )
}

#[cfg(unix)]
fn stdout_flood() -> (&'static str, Vec<OsString>) {
    (
        "sh",
        vec![
            OsString::from("-c"),
            OsString::from("head -c 2000000 /dev/zero"),
        ],
    )
}

#[cfg(not(unix))]
fn stdout_flood() -> (&'static str, Vec<OsString>) {
    (
        "powershell",
        vec![
            OsString::from("-NoProfile"),
            OsString::from("-Command"),
            OsString::from("Write-Output ('x' * 2000000)"),
        ],
    )
}

#[cfg(unix)]
fn stderr_flood() -> (&'static str, Vec<OsString>) {
    (
        "sh",
        vec![
            OsString::from("-c"),
            OsString::from("head -c 2000000 /dev/zero >&2"),
        ],
    )
}

#[cfg(not(unix))]
fn stderr_flood() -> (&'static str, Vec<OsString>) {
    (
        "powershell",
        vec![
            OsString::from("-NoProfile"),
            OsString::from("-Command"),
            OsString::from("[Console]::Error.Write(('x' * 2000000) -join '')"),
        ],
    )
}

#[cfg(unix)]
fn small_answer() -> (&'static str, Vec<OsString>) {
    (
        "sh",
        vec![
            OsString::from("-c"),
            OsString::from("printf '{\"ok\":true}'"),
        ],
    )
}

#[cfg(not(unix))]
fn small_answer() -> (&'static str, Vec<OsString>) {
    (
        "powershell",
        vec![
            OsString::from("-NoProfile"),
            OsString::from("-Command"),
            OsString::from("Write-Output '{\"ok\":true}'"),
        ],
    )
}

/// A hung Cargo must be killed by the deadline, not waited out, and the
/// refusal must name the deadline rather than decoding a partial graph
/// (issue #143 R14).
#[test]
fn a_hung_subprocess_is_killed_by_the_deadline() -> TestResult {
    let (program, args) = sleeper();
    let timeout = Duration::from_millis(500);
    let started = std::time::Instant::now();
    let refused = run_bounded(program, &args, Path::new("."), timeout, 1024 * 1024);
    let elapsed = started.elapsed();
    match refused {
        Err(MetadataError::Timeout { after }) => assert_eq!(after, timeout),
        Err(other) => return Err(format!("expected a timeout refusal, got {other}").into()),
        Ok(_) => return Err("a hung child must not report success".into()),
    }
    // Overshoot is observed, not assumed: the deadline is checked every
    // 5 ms poll quantum and the kill and reap follow at once, so anything
    // near a second past the deadline is a defect, not scheduling noise.
    let overshoot = elapsed.saturating_sub(timeout);
    assert!(
        overshoot < Duration::from_millis(1500),
        "the deadline must kill the 10s sleeper promptly: {overshoot:?} past a {timeout:?} deadline"
    );
    Ok(())
}

/// INV-DEP-8's stated scope, observed with a real descendant: the direct
/// child exits at once after starting a grandchild that inherits the
/// capture descriptors. Collection must return without waiting for the
/// grandchild, and every capture name must be gone, while the grandchild
/// is still alive holding its descriptor. Disk space for an unlinked file
/// is reclaimed only when that last descriptor closes, which is exactly
/// the limit the invariant declares. The grandchild's PID comes from its
/// own output, so the only process signalled is the one this test made.
#[cfg(unix)]
#[test]
fn a_descendant_holding_a_capture_neither_blocks_collection_nor_keeps_its_name() -> TestResult {
    arm_faults(&[]);
    let started = std::time::Instant::now();
    let output = run_bounded(
        "sh",
        &[OsString::from("-c"), OsString::from("sleep 30 & echo $!")],
        Path::new("."),
        Duration::from_secs(10),
        32 * 1024,
    )?;
    let elapsed = started.elapsed();
    let pid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert!(
        !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()),
        "the direct child must report its grandchild's pid: {pid:?}"
    );
    let alive = |pid: &str| -> std::io::Result<bool> {
        Ok(Command::new("kill").args(["-0", pid]).status()?.success())
    };
    let grandchild_was_alive = alive(&pid)?;
    let _signalled = Command::new("kill").args(["-9", &pid]).status();
    assert!(
        elapsed < Duration::from_secs(5),
        "collection must not wait for a descendant holding the capture: {elapsed:?}"
    );
    assert!(
        grandchild_was_alive,
        "the grandchild must still hold its descriptor when collection returns"
    );
    assert_paths_removed(&captured_paths())
}

/// A flooding stdout is refused past its budget instead of retained.
#[test]
fn a_flooding_stdout_is_refused_past_its_budget() -> TestResult {
    let (program, args) = stdout_flood();
    let refused = run_bounded(
        program,
        &args,
        Path::new("."),
        Duration::from_secs(30),
        32 * 1024,
    );
    assert_output_budget_refusal(refused, "stdout")
}

/// A flooding stderr is refused past its budget instead of retained.
#[test]
fn a_flooding_stderr_is_refused_past_its_budget() -> TestResult {
    let (program, args) = stderr_flood();
    let refused = run_bounded(
        program,
        &args,
        Path::new("."),
        Duration::from_secs(30),
        32 * 1024,
    );
    assert_output_budget_refusal(refused, "stderr")
}

/// A flood on either stream is refused at its budget, naming the stream.
///
/// One assertion for both families: the stdout and stderr cases differ only
/// in which capture floods, and two copies of the check are two things that
/// can drift into testing different budgets.
fn assert_output_budget_refusal(
    refused: Result<BoundedOutput, MetadataError>,
    stream: &str,
) -> TestResult {
    match refused {
        Err(MetadataError::OutputTooLarge {
            stream: refused_stream,
            limit,
        }) => {
            assert_eq!(refused_stream, stream);
            assert_eq!(limit, 32 * 1024);
            Ok(())
        }
        Err(other) => {
            Err(format!("expected an output-budget refusal on {stream}, got {other}").into())
        }
        Ok(_) => Err(format!("a flood on {stream} must not report success").into()),
    }
}

/// The control: a small healthy answer passes through unchanged.
#[test]
fn a_small_answer_passes_through_unchanged() -> TestResult {
    let (program, args) = small_answer();
    let output = run_bounded(
        program,
        &args,
        Path::new("."),
        Duration::from_secs(30),
        32 * 1024,
    )?;
    assert!(
        output.status.success(),
        "the probe command must succeed: {:?}",
        output.status
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("{\"ok\":true}"),
        "the probe bytes must survive: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    Ok(())
}

/// A capture name stays owned across a subsequent early return and its
/// actual temporary file is reclaimed by the RAII fallback.
#[test]
fn an_owned_capture_is_removed_on_early_return() -> TestResult {
    use std::fs::OpenOptions;

    let token = lgwks_std::random::bytes::<16>()?;
    let path = std::env::temp_dir().join(format!(
        "lgwks-deps-test-{}.capture",
        lgwks_std::hex::encode(token)
    ));
    let _file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let mut captures = CaptureFiles::new();
    captures.own(path.clone());
    drop(_file);
    drop(captures);
    assert_eq!(
        std::fs::metadata(&path).err().map(|error| error.kind()),
        Some(std::io::ErrorKind::NotFound),
        "the acquired capture pathname must be reclaimed on scope exit"
    );
    Ok(())
}

/// Dropping an unresolved obligation kills and reaps its child, so a
/// refusal the caller only prints does not leave Cargo running.
#[cfg(unix)]
#[test]
fn a_dropped_obligation_kills_and_reaps_its_child() -> TestResult {
    let child = std::process::Command::new("sleep").arg("30").spawn()?;
    let pid = child.id().to_string();
    drop(CleanupObligation::new(
        Some(CleanupStep::Kill),
        Some(child),
        &mut CaptureFiles::new(),
    ));
    // `kill -0` succeeds for any process that still has a table entry,
    // zombies included, so a failure here means killed *and* reaped.
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid])
        .stderr(std::process::Stdio::null())
        .status()?;
    assert!(
        !alive.success(),
        "the dropped obligation's child {pid} is still in the process table"
    );
    Ok(())
}

/// An already-exited real child is cleared only after the OS reports its
/// status; a failed kill is not itself treated as proof of absence.
#[test]
fn cleanup_obligation_confirms_an_already_exited_child() -> TestResult {
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("--list")
        .spawn()?;
    let _status = child.wait()?;
    let mut obligation = CleanupObligation::new(
        Some(CleanupStep::Kill),
        Some(child),
        &mut CaptureFiles::new(),
    );
    assert!(
        obligation.retry_cleanup()?,
        "observed process absence and empty capture ownership complete cleanup"
    );
    Ok(())
}

/// Every fallible collection stage retains and resolves the resources it
/// already acquired. The injected errors are single-use and thread-local;
/// the process and capture files are real operating-system resources.
#[test]
fn injected_collection_fault_matrix_keeps_cleanup_owned_and_truthful() -> TestResult {
    /// A ceiling, never the property under test: each injected fault fires
    /// on its first observation, and the small answer must be allowed to
    /// exit first. On hosted Windows that answer is a cold PowerShell start,
    /// which took longer than the 3 s this used to allow, so the deadline
    /// refusal landed before `ReadStdout` was reached. The same 30 s the
    /// passthrough control allows.
    const CEILING: Duration = Duration::from_secs(30);
    let (small_program, small_args) = small_answer();
    let (sleep_program, sleep_args) = sleeper();
    let stages = [
        (FaultPoint::CaptureStdout, false),
        (FaultPoint::CaptureStderr, false),
        (FaultPoint::Spawn, false),
        (FaultPoint::WaitObservation, true),
        (FaultPoint::StatStdout, true),
        (FaultPoint::StatStderr, true),
        (FaultPoint::ReadStdout, false),
        (FaultPoint::ReadStderr, false),
    ];
    for (fault, running_child) in stages {
        arm_faults(&[fault]);
        let (program, args) = if running_child {
            (sleep_program, sleep_args.as_slice())
        } else {
            (small_program, small_args.as_slice())
        };
        let result = run_bounded(program, args, Path::new("."), CEILING, 1024 * 1024);
        let error = match result {
            Err(error) => error,
            Ok(_) => return Err(format!("fault {fault:?} unexpectedly passed").into()),
        };
        assert!(
            matches!(error, MetadataError::Spawn(_)),
            "fault {fault:?} should remain the primary collection error: {error}"
        );
        let paths = captured_paths();
        assert_paths_removed(&paths)?;
        assert!(
            FAULTS.with(|faults| faults.borrow().is_empty()),
            "fault {fault:?} was not reached"
        );
    }

    for failure in [FaultPoint::Kill, FaultPoint::Reap] {
        arm_faults(&[FaultPoint::WaitObservation, failure]);
        let error = match run_bounded(
            sleep_program,
            &sleep_args,
            Path::new("."),
            CEILING,
            1024 * 1024,
        ) {
            Err(error) => error,
            Ok(_) => return Err(format!("fault {failure:?} unexpectedly passed").into()),
        };
        let text = error.to_string();
        let MetadataError::ProcessCleanup {
            process,
            mut obligation,
            ..
        } = error
        else {
            return Err(format!("fault {failure:?} did not expose an obligation").into());
        };
        assert!(process.is_some(), "fault {failure:?} must remain visible");
        let verb = if failure == FaultPoint::Kill {
            "kill"
        } else {
            "reap"
        };
        let pid = obligation
            .report
            .pid
            .ok_or("an owned child keeps its pid in the report")?;
        assert!(
            text.contains(&format!("could not {verb} cargo (pid {pid}): ")),
            "the refusal names the step, the pid and the OS error: {text}"
        );
        assert!(
            retry_until_resolved(&mut obligation)?,
            "retry should finish owned cleanup"
        );
        assert_paths_removed(&captured_paths())?;
        assert!(FAULTS.with(|faults| faults.borrow().is_empty()));
    }

    // Output read in full and child reaped, then one unlink fails: the
    // output is returned, and the failure is reported beside it (#193).
    arm_faults(&[FaultPoint::Unlink]);
    let output = run_bounded(
        small_program,
        &small_args,
        Path::new("."),
        CEILING,
        1024 * 1024,
    )
    .map_err(|error| format!("a complete collection was refused over cleanup: {error}"))?;
    assert!(output.status.success(), "the child's own verdict is kept");
    assert!(!output.stdout.is_empty(), "the complete output is kept");
    let unresolved = output
        .unresolved
        .ok_or("the unlink failure must be reported beside the output")?;
    let error = unresolved.into_error(None);
    let text = error.to_string();
    let MetadataError::ProcessCleanup {
        cause,
        captures,
        mut obligation,
        ..
    } = error
    else {
        return Err("unlink fault did not expose an unresolved cleanup owner".into());
    };
    assert!(cause.is_none(), "nothing was refused before the cleanup");
    assert_eq!(captures.len(), 1, "the unlink failure is retained");
    let retained = obligation.captures.paths.clone();
    assert_eq!(retained.len(), 1, "the unresolved path stays owned");
    assert!(
        text.contains(&format!("could not remove {}: ", retained[0].display())),
        "the report names the path it could not remove: {text}"
    );
    assert!(std::fs::metadata(&retained[0]).is_ok());
    assert!(retry_until_resolved(&mut obligation)?);
    assert_paths_removed(&retained)?;
    assert!(FAULTS.with(|faults| faults.borrow().is_empty()));
    Ok(())
}

/// A refusal decided after a complete read (Cargo's non-zero status, a
/// JSON or schema error) keeps an unresolved capture cleanup attached, so
/// neither the refusal nor the leftover path is lost.
#[test]
fn a_refusal_after_collection_keeps_the_unresolved_cleanup_attached() -> TestResult {
    let path = std::env::temp_dir().join(format!(
        "lgwks-deps-attach-{}.capture",
        lgwks_std::hex::encode(lgwks_std::random::bytes::<8>()?)
    ));
    std::fs::write(&path, b"{}")?;
    let mut captures = CaptureFiles::new();
    captures.own(path.clone());
    let collected = Collected {
        value: (),
        unresolved: Some(Unresolved {
            captures: vec![std::io::Error::other("injected unlink failure")],
            obligation: Box::new(CleanupObligation::new(None, None, &mut captures)),
        }),
    };
    let refusal = collected.refuse(MetadataError::Cargo("refused".to_owned()));
    let text = refusal.to_string();
    let MetadataError::ProcessCleanup {
        cause,
        mut obligation,
        ..
    } = refusal
    else {
        return Err("the cleanup failure was dropped by the refusal".into());
    };
    assert!(
        cause.as_deref().is_some_and(
            |error| matches!(*error, MetadataError::Cargo(ref message) if message == "refused")
        ),
        "the refusal is the primary cause"
    );
    assert!(
        text.starts_with("cargo metadata refused: refused; cleanup unconfirmed; could not remove "),
        "both are reported, refusal first: {text}"
    );
    assert!(
        obligation.retry_cleanup()?,
        "the retained owner still removes the path"
    );
    assert_paths_removed(&[path])?;
    Ok(())
}
