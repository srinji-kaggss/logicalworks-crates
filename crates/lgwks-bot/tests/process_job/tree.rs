//! The process trees the Windows containment tests start, and what they read
//! back about them.
//!
//! A job object's membership list is the supervisor's own evidence: the receipt
//! says the job read empty, and the code that terminates the job is the code that
//! reads it. These tests also read a fact the supervisor does not produce. Every
//! background `ping` redirects its output into a file of its own, and `cmd.exe`
//! opens a redirect target without `FILE_SHARE_DELETE`, so the system refuses to
//! delete that file for as long as any process holds a handle to it. A file that
//! deletes is a descendant that is gone, read from the handle table and not from
//! the job.

use std::error::Error;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use lgwks_bot::rt::supervise::{CleanupReceipt, ContainmentMechanism, ShutdownReport, TaskOutcome};
use lgwks_bot::rt::time::{Instant, sleep};

use super::BUDGET;

/// The file the shell writes once every background process has been started.
pub const UP: &str = "up.txt";

/// A script tail that outlives every test: the shell stays until it is stopped.
pub const STAY: &str = "ping -n 600 127.0.0.1 >nul";

/// The file background process `index` holds open.
fn held_name(index: usize) -> String {
    format!("held-{index}.txt")
}

/// A shell script that starts `held` background pings, each holding a file of its
/// own, says so in [`UP`], and then runs `tail`.
pub fn script(held: usize, tail: &str) -> String {
    (0..held)
        .map(|index| format!("start /b ping -n 600 127.0.0.1 >{}", held_name(index)))
        .chain([format!("echo up> {UP}"), String::from(tail)])
        .collect::<Vec<_>>()
        .join(" & ")
}

/// A script tail that ends the shell by itself with `code`, after a second.
///
/// The second is the shell's own work and nothing a test waits for: it keeps the
/// shell alive while the supervisor assigns it to its job, which a shell that
/// exited at once could beat on a stalled host. No assertion reads the clock.
pub fn exit_after_a_second(code: u8) -> String {
    format!("ping -n 2 127.0.0.1 >nul & exit {code}")
}

/// Whether some process still holds `held-<index>.txt` open: the file is there
/// and the system refuses to delete it.
///
/// A file that does delete is gone for good, so every later question about it
/// answers "not held". That is the right reading after a stop, and it is why the
/// question is asked of a tree that is still running only once.
pub fn is_held(dir: &Path, index: usize) -> bool {
    let path = dir.join(held_name(index));
    path.exists() && std::fs::remove_file(&path).is_err()
}

/// Whether every one of the `held` files is held now.
pub fn all_held(dir: &Path, held: usize) -> bool {
    (0..held).all(|index| is_held(dir, index))
}

/// Wait until no process holds any of the `held` files, up to [`BUDGET`].
///
/// Every file is asked on every pass, so one that has been released is deleted
/// and stays released while the others are waited for.
pub async fn all_released(dir: &Path, held: usize) -> bool {
    let started = Instant::now();
    loop {
        if (0..held).filter(|index| is_held(dir, *index)).count() == 0 {
            break true;
        }
        if started.elapsed() >= BUDGET {
            break false;
        }
        sleep(Duration::from_millis(10)).await;
    }
}

/// Block this thread, without yielding, until the shell has written [`UP`].
///
/// There is no await in it on purpose. On a current-thread runtime a task the
/// test has just started cannot run until the test yields, so a stop requested
/// after this call is requested before that task has run at all. The shell is
/// another process and runs on regardless.
pub fn up_without_yielding(dir: &Path) -> Result<(), Box<dyn Error>> {
    let started = std::time::Instant::now();
    loop {
        if dir.join(UP).exists() {
            break Ok(());
        }
        if started.elapsed() >= BUDGET {
            break Err(format!("the shell never wrote {UP}").into());
        }
        std::hint::spin_loop();
    }
}

/// `Ok` when `holds`, and `what` as the failure otherwise.
pub fn require(holds: bool, what: &str) -> Result<(), String> {
    if holds {
        Ok(())
    } else {
        Err(String::from(what))
    }
}

/// What the receipt of a stopped tree says, or the first thing it fails to say.
///
/// `at_least` is how many processes the job must have read before its terminate.
/// It is a floor and never an equality, because a console host can join the job
/// on some images. The text returned holds only facts that do not vary with the
/// host, so a simulation may hash it.
pub fn settled(outcome: &TaskOutcome, at_least: usize) -> Result<String, String> {
    let containment = outcome
        .containment()
        .ok_or_else(|| String::from("a stopped process carries a containment report"))?;
    let claims = [
        (
            outcome.cleanup() == Some(&CleanupReceipt::CleanupConfirmed),
            "the job was not confirmed empty",
        ),
        (
            containment.mechanism() == ContainmentMechanism::JobObject,
            "the mechanism was not a job object",
        ),
        (
            containment.captured() >= at_least,
            "the job held fewer processes than were started",
        ),
        (
            containment.signalled() == containment.captured(),
            "the terminate did not reach every member that was read",
        ),
        (
            containment.survivors().is_empty(),
            "the receipt names a survivor",
        ),
        (
            containment.is_complete(),
            "the containment report is not complete",
        ),
    ];
    match claims.iter().find(|claim| !claim.0) {
        None => Ok(String::from("confirmed job-object whole")),
        Some(broken) => Err(format!(
            "{}: receipt {:?}, captured {}, signalled {}, survivors {:?}",
            broken.1,
            outcome.cleanup(),
            containment.captured(),
            containment.signalled(),
            containment.survivors()
        )),
    }
}

/// Whether `outcome` is the shell's own exit with `code`: a success for zero, and
/// otherwise a process failure that names that code.
pub fn exited_with(outcome: &TaskOutcome, code: u8) -> Result<(), String> {
    let reported = outcome.exit_status().and_then(|status| status.code());
    let right = if code == 0 {
        matches!(*outcome, TaskOutcome::Completed { .. })
    } else {
        outcome.is_process_failure() && reported == Some(i32::from(code))
    };
    if right {
        Ok(())
    } else {
        Err(format!(
            "expected the shell to exit {code}, got {outcome:?}"
        ))
    }
}

/// The receipt of the one tree a shutdown drained: exactly one outcome, reported
/// cancelled, whose job settled as [`settled`] reads it with `held` background
/// processes behind a shell.
pub fn stopped_tree(report: &ShutdownReport, held: usize) -> Result<String, String> {
    let outcomes = report.outcomes();
    require(outcomes.len() == 1, "one tree must drain as one outcome")?;
    let drained = outcomes
        .first()
        .ok_or_else(|| String::from("the shutdown reported no outcome"))?;
    require(
        matches!(*drained, TaskOutcome::Cancelled { .. }),
        "a stopped tree is reported cancelled",
    )?;
    settled(drained, held.saturating_add(1))
}

/// A process the test started on its own, outside every job: the unrelated
/// process that no stop of a supervised tree may reach.
pub struct Outsider(Child);

impl Outsider {
    /// Start a `ping` that outlives every test, with no stream shared with it.
    pub fn start() -> std::io::Result<Self> {
        Command::new("ping")
            .args(["-n", "600", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(Self)
    }

    /// Whether it is still running: it has not exited and its state is readable.
    pub fn is_running(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

impl Drop for Outsider {
    /// Stop and collect it, so a failed test leaves no process behind.
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}
