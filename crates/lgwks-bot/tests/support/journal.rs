//! Shared fixtures for the journal liveness and scale test families.
//!
//! One home for the run identity, the scratch-path and cleanup guards, and the
//! attempt-key constructor, so the two families cannot drift into writing
//! different facts under what looks like the same key.
//!
//! Included with `#[path = "support/journal.rs"] mod shared;` rather than
//! declared as its own test target: it holds no `#[test]`, only the scaffolding
//! both targets need.
#![allow(
    dead_code,
    reason = "each test target that includes this module uses a different subset of it"
)]

use std::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};

use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectKey, EnvironmentEpoch, EnvironmentId, FlowRevision,
    RunId,
};
use lgwks_bot::journal::EffectJournal as _;

/// The run every fixture key belongs to.
pub const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
/// The action every fixture key names.
pub const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
/// The environment every fixture key is fenced against.
pub const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
/// The flow revision every fixture key was produced from.
pub const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
/// The input digest every fixture key binds.
pub const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

/// A counter that gives concurrent test runs distinct scratch names.
///
/// The process id, the nanos and a counter, each covering what the others
/// cannot. The counter is per process and the clock resolves to microseconds on
/// macOS, so two test processes started together — the local gate runs the same
/// test from two lanes at once — derived the same name and one opened the
/// other's journal (`Locked`). A process id is unique among *live* processes,
/// which are the ones that can collide; the nanos separate a pid the OS reuses
/// later, and the counter separates calls within one process.
static SCRATCH: AtomicU64 = AtomicU64::new(0);

/// A scratch path unique to one test run.
pub fn scratch(name: &str) -> std::path::PathBuf {
    let unique = SCRATCH.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    let process = std::process::id();
    std::env::temp_dir().join(format!("lgwks-journal-{name}-{process}-{nanos}-{unique}"))
}

/// A scratch *directory* unique to one test run, created before it is returned.
///
/// Separate from [`scratch`] because the two uses are genuinely different: a
/// caller that hands the path to a journal opening it itself needs no directory,
/// and a caller that writes files into it needs one made. Naming both is cheaper
/// than a boolean, and it keeps the `?` at the call site.
pub fn scratch_dir(name: &str) -> std::io::Result<std::path::PathBuf> {
    let dir = scratch(name);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Removes a test's scratch path when the test ends, however it ends.
pub struct TempGuard(pub std::path::PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.0.is_dir() {
            drop(std::fs::remove_dir_all(&self.0));
        } else {
            drop(std::fs::remove_file(&self.0));
        }
    }
}

/// Append one measurement line to the file named by `LGWKS_SCALE_OUT`.
///
/// The measurement targets write their tails and tier lines here rather than to
/// stdout, because the workspace forbids printing and because a report needs the
/// number the run produced, not an assertion that happened to pass. The file is
/// opened in append mode so two measurement targets that share it cannot erase
/// each other's lines, and an unset variable is a silent no-op so the tests pass
/// unchanged on a host that does not want a report.
pub fn record_measurement(line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let Some(path) = std::env::var_os("LGWKS_SCALE_OUT") else {
        return Ok(());
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")
}

/// Walk one attempt up its whole ladder, every rung acknowledged in order.
///
/// One function for every harness that walks a complete attempt, because the
/// ladder is the journal's ordering rule and a copy that walks a *different*
/// four rungs is not a shorter spelling of this one — it is a second claim about
/// what an attempt looks like. `predicate` and `version` name the verification's
/// predicate, so two callers verifying the same attempt with different predicates
/// do not collide.
pub fn ladder(
    key: lgwks_bot::effect::EffectKey,
    predicate: &str,
    version: u64,
) -> Result<Vec<lgwks_bot::journal::EffectEvent>, Box<dyn Error>> {
    use lgwks_bot::effect::Id128;
    use lgwks_bot::journal::{EffectEvent, EffectEvidence, Verification, VerificationResult};

    Ok(vec![
        EffectEvent::IntentAdmitted { key },
        EffectEvent::DispatchPrepared { key },
        EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        },
        EffectEvent::Verified {
            key,
            verification: Verification::new(
                Id128::from_hex(predicate)?,
                version,
                lgwks_std::hash::blake3(b"the durable ladder's own predicate"),
                VerificationResult::Satisfied,
            ),
        },
    ])
}

/// Append the first `rungs` of [`ladder`], each at the journal's own tail.
///
/// One function for every harness that walks an attempt, because the ladder is
/// the journal's ordering rule and a copy that walks different rungs is not a
/// shorter spelling of this one — it is a second claim about what an attempt
/// looks like. A row that kills at the second rung walks the same first two
/// rungs a complete walk would, which is what makes "the kill landed here" a
/// statement about the ladder rather than about the fixture.
pub fn walk_ladder(
    journal: &mut lgwks_bot::journal::FileJournal,
    key: lgwks_bot::effect::EffectKey,
    predicate: &str,
    version: u64,
    rungs: usize,
) -> Result<(), Box<dyn Error>> {
    for event in ladder(key, predicate, version)?.into_iter().take(rungs) {
        let tail = journal.tail();
        journal.compare_and_append(tail, &event)?;
    }
    Ok(())
}

/// A fixture key for attempt `attempt`, under the shared identity.
pub fn key(attempt: u64) -> Result<EffectKey, Box<dyn Error>> {
    key_for(&attempt.to_string(), DIGEST_HEX)
}

/// The key for attempt `attempt` under the shared identity, binding `digest`.
///
/// `digest` is the attempt's content identity; a different digest is a
/// different attempt with its own ladder, which is exactly the distinction the
/// crash rows turn on. Every harness that journals under the shared run builds
/// its key here, so two harnesses cannot fold different worlds under what
/// looks like the same key.
pub fn key_for(attempt: &str, digest: &str) -> Result<EffectKey, Box<dyn Error>> {
    let run = RunId::from_hex(RUN)?;
    let action = ActionId::from_hex(ACTION)?;
    let attempt = AttemptId::from_decimal(attempt)?;
    let flow = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
    let digest = ActionDigest::from_tagged("blake3_256", digest)?;
    let environment = EnvironmentId::from_hex(ENV)?;
    let epoch = EnvironmentEpoch::from_decimal("1")?;
    Ok(EffectKey::new(
        run,
        action,
        attempt,
        flow,
        digest,
        environment,
        epoch,
    ))
}

/// A plain-process pause, for the kill harnesses' two branches that have no
/// runtime: a probe child parking until it is killed, and the parent polling
/// for the child's marker.
///
/// The workspace bans `std::thread::sleep` because blocking an executor thread
/// stalls every task on it. Neither branch here has an executor, and the wait —
/// a child parked while the parent decides when it dies — is the observation's
/// subject rather than its scaffolding.
#[expect(
    clippy::disallowed_methods,
    reason = "the kill harness is a plain process with no async runtime and no reactor to \
              stall; the parked child and the marker poll are the observation's shape, and \
              `rt::time::sleep` cannot be awaited here"
)]
pub fn pause(millis: u64) {
    std::thread::sleep(std::time::Duration::from_millis(millis));
}

/// Owns a probe child and kills it, reaping it, however the test ends.
///
/// A test that returns early, or panics, between the spawn and the kill must
/// not leave a live child behind: the drop is the backstop the harness itself
/// can forget. `take` hands the child to the kill harness, which then owns the
/// kill and the reap, so the guard drops empty and no path kills twice.
pub struct ProbeGuard(pub Option<std::process::Child>);

impl ProbeGuard {
    /// Hand the child to the kill harness.
    pub fn take(&mut self) -> Option<std::process::Child> {
        self.0.take()
    }

    /// Wait until `marker` exists — the child's proof that what it wrote was
    /// acknowledged — then kill the child with a real `SIGKILL` and reap it.
    ///
    /// `Child::kill` sends `SIGKILL` on Unix: no cleanup, no destructors, no
    /// flushing. Whatever the child wrote that is not on the disk is gone, and
    /// whatever claimed to be durable had better be there. The wait is bounded,
    /// so a child that never reaches its marker cannot leave a stray process.
    pub fn kill_after_marker(
        &mut self,
        marker: &std::path::Path,
        test_name: &str,
    ) -> Result<(), Box<dyn Error>> {
        let Some(mut child) = self.take() else {
            {
                let refusal =
                    Err(format!("the probe child for {test_name} was gone before the kill").into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "kill_after_marker: returning an error to the caller");
                return refusal;
            };
        };
        for _ in 0..2_000 {
            if marker.exists() {
                child.kill()?;
                let status = child.wait()?;
                // The kill, not an earlier failure, must be what ended the
                // child: a probe that died on its own after writing the marker
                // would make the observation a courtesy, not a kill.
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt as _;
                    assert_eq!(
                        status.signal(),
                        Some(9),
                        "the probe for {test_name} must have been killed, not exited: {status}"
                    );
                }
                return Ok(());
            }
            if let Some(status) = child.try_wait()? {
                let refusal = Err(format!(
                    "the probe child for {test_name} exited on its own before the kill: {status}"
                )
                .into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "kill_after_marker: returning an error to the caller");
                return refusal;
            }
            pause(5);
        }
        child.kill()?;
        child.wait()?;
        Err(format!(
            "the probe child for {test_name} never reached its marker; the observation has no \
             kill to test"
        )
        .into())
    }
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            drop(child.kill());
            drop(child.wait());
        }
    }
}
