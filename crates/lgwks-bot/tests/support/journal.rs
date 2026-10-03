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
/// Nanos plus a counter, not a process id: the OS reuses both pids and threads,
/// and a reused id must never make two runs share a journal.
static SCRATCH: AtomicU64 = AtomicU64::new(0);

/// A scratch path unique to one test run.
pub fn scratch(name: &str) -> std::path::PathBuf {
    let unique = SCRATCH.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("lgwks-journal-{name}-{nanos}-{unique}"))
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

/// A fixture key for attempt `attempt`, under the shared identity.
pub fn key(attempt: u64) -> Result<EffectKey, Box<dyn Error>> {
    Ok(EffectKey::new(
        RunId::from_hex(RUN)?,
        ActionId::from_hex(ACTION)?,
        AttemptId::from_decimal(&attempt.to_string())?,
        FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
        ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?,
        EnvironmentId::from_hex(ENV)?,
        EnvironmentEpoch::from_decimal("1")?,
    ))
}
