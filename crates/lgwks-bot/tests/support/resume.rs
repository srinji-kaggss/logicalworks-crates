//! The scaffolding the durable resume families need, written once.
//!
//! `task_resume.rs`, `sim_task_resume.rs`, the store-fault and replay-drift
//! families and the request-key families drive the same store over the same
//! filesystem and needed the same things: a task whose whole body is one
//! `remember`, a marker file that counts body runs across a kill, the store
//! file inside a run's scratch directory, typed readers for the store's
//! refusals, and a percentile summary. A copy per file is how they drift. The
//! scratch directory itself is `scratch.rs`, shared with every other family.
//!
//! Every consumer is a durable run, which needs both `script` (the task body)
//! and `ephemeral` (the run identity a store host mints), so the whole file is
//! included under both. The parked-device instrument the journal liveness
//! target also needs lives in `liveness.rs`, under `rt` alone.

use std::error::Error;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{Task, task};

/// The named future a task body returns, so a body has one type every call site can
/// pass to a [`Task`].
///
/// `Pin<Box<dyn Future>>` rather than the concrete anonymous future: the point of
/// these files is to prove what the store and the report do, and a boxed future is
/// the only spelling of "a task body" that can be named in a signature.
pub type BodyFuture = Pin<Box<dyn Future<Output = Result<u32, FlowError>>>>;

/// The task type both files build over [`BodyFuture`].
pub type OneStep = Task<fn(Scope, u32) -> BodyFuture>;

/// The marker file naming one step's body.
///
/// A file rather than an in-memory counter because the claims these support are
/// about a body that was *not* polled, and a counter in the process that would
/// have polled it is a claim about nothing. The file also survives the reopen a
/// resume performs, which is the whole of the observation.
pub fn marker(dir: &Path, step: &str) -> PathBuf {
    dir.join(format!("{step}.ran"))
}

/// How many times a step's body has run, as the filesystem records it.
pub fn ran(dir: &Path, step: &str) -> Result<u32, Box<dyn Error>> {
    match std::fs::read_to_string(marker(dir, step)) {
        Ok(text) => Ok(text.trim().parse::<u32>()?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

/// Append one to a step's marker. The file is the count, so it survives the kill.
pub fn record_run(dir: &Path, step: &str) -> Result<(), FlowError> {
    let next = ran(dir, step).map_err(FlowError::failed)?.saturating_add(1);
    std::fs::write(marker(dir, step), next.to_string()).map_err(FlowError::failed)
}

/// A task whose whole body is one `remember` call over a caller-chosen value.
///
/// One task rather than one per test, because the durable claim is about the
/// `remember` and not about the body around it, and two differently-shaped bodies
/// would be two different things being measured.
pub fn one_step_task() -> Result<OneStep, FlowError> {
    task("step", boxed_body)
}

/// The one-step body behind its nameable return type.
pub fn boxed_body(scope: Scope, value: u32) -> BodyFuture {
    Box::pin(
        async move { remember(&scope, "v", || async move { Ok::<_, FlowError>(value) }).await },
    )
}

/// The two versions a run store's format refusal names, or `None` for any other
/// refusal.
///
/// One reader for three test binaries that all need to tell a `\x01` store's
/// refusal from every other one. It exists as a function rather than a pattern at
/// each call site for two reasons. The workspace forbids
/// `clippy::pattern_type_mismatch`, so every way of reaching a field out of a
/// borrowed error is a lint error in one direction or the other and the pattern
/// has to be rewritten per ownership. And a `match` on [`StoreError`] in three
/// files is three copies of the same discrimination — which is exactly the block
/// the commit guard refuses, and which is what makes one of them quietly accept
/// a different arm if the enum grows.
///
/// The pair is `(found, expected)` rather than the error itself so a caller can
/// assert on the two numbers without re-matching.
#[must_use]
pub fn format_version(error: &lgwks_bot::task::StoreError) -> Option<(u8, u8)> {
    match error {
        &lgwks_bot::task::StoreError::FormatVersion { found, expected } => Some((found, expected)),
        _ => None,
    }
}

/// The store's own refusal inside a flow error, or `None` for any other arm.
///
/// The typed half of the read-failure claim (INV-BOT-7): a store that cannot be
/// read must reach the caller as `FlowError::Store` wrapping the store's own
/// error, not as a `Failed` string and never as a definition drift. A test that
/// matched a rendering would pass on text that happened to mention a device
/// while the payload was something else, so the discrimination is on the variant
/// and on the wrapped [`StoreError`](lgwks_bot::task::StoreError) itself.
///
/// One function rather than a `match` per call site: the workspace forbids
/// `pattern_type_mismatch`, so every way of reaching the boxed source out of a
/// borrowed error is a lint error in one direction or the other, and three
/// copies of the same discrimination is the block the commit guard refuses.
#[must_use]
pub fn store_refusal(error: &FlowError) -> Option<&lgwks_bot::task::StoreError> {
    match *error {
        FlowError::Store { ref source, .. } => Some(&**source),
        _ => None,
    }
}

/// The disposition as a pair of numbers, for a trace that compares runs.
///
/// A rendered label changes when the enum gains an arm; the two numbers below do
/// not, so a seeded scenario that recorded the label would report a trace
/// difference for a change that never affected what the scenario decided.
#[must_use]
pub fn disposition_code(disposition: lgwks_bot::task::Disposition) -> u64 {
    match disposition {
        lgwks_bot::task::Disposition::Succeeded => 0,
        lgwks_bot::task::Disposition::Failed => 1,
        lgwks_bot::task::Disposition::Cancelled => 2,
        lgwks_bot::task::Disposition::DeadlineExceeded => 3,
        lgwks_bot::task::Disposition::Refused => 4,
        _ => 255,
    }
}

/// The store file a scenario opens directly, inside its own scratch directory.
///
/// One name for every family: the directory is already unique to the run, so
/// the file only has to be findable, and a host built over the same directory
/// names its own file after the tenant, which no family calls `runs`.
#[must_use]
pub fn store_file(dir: &Path) -> PathBuf {
    dir.join("runs.runstore")
}

/// The percentiles of one measurement, in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Median.
    pub p50: u128,
    /// 95th.
    pub p95: u128,
    /// 99th.
    pub p99: u128,
}

impl Summary {
    /// The percentiles of `samples`, sorted in place, or `None` for an empty
    /// sample: nothing measured has no percentile, and a zero would claim one.
    #[must_use]
    pub fn of(samples: &mut [u128]) -> Option<Self> {
        use crate::journal_fixtures::percentile;
        samples.sort_unstable();
        Some(Self {
            p50: percentile(samples, 500)?,
            p95: percentile(samples, 950)?,
            p99: percentile(samples, 990)?,
        })
    }

    /// The summary as one line, for a harness that reports rather than asserts.
    #[must_use]
    pub fn line(&self, label: &str) -> String {
        format!(
            "{label}: p50={}us p95={}us p99={}us",
            self.p50, self.p95, self.p99
        )
    }
}

/// A resident-set size as MiB, or the honest statement that nothing reports it.
///
/// "unavailable on this platform" rather than a zero: a measurement that reports
/// zero where it measured nothing is the one number a reader must not be shown.
#[must_use]
pub fn peak_rss_mib() -> String {
    const MIB: u128 = 1024 * 1024;
    peak_rss_bytes().map_or_else(
        || String::from("unavailable on this platform"),
        |bytes| format!("{} MiB", u128::from(bytes).div_ceil(MIB)),
    )
}

/// Peak resident set size of this process, or `None` where nothing reports it.
///
/// `/proc/self/status` only, and `None` — never a fabricated number — elsewhere.
/// A machine that cannot answer the question must not have the test invent one.
#[must_use]
fn peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kib: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
            return Some(kib.saturating_mul(1024));
        }
    }
    None
}
