//! The scaffolding both resume test files need, written once.
//!
//! `task_resume.rs` and `resume_liveness.rs` drive the same store over the same
//! filesystem and needed the same four things: a task whose whole body is one
//! `remember`, a scratch directory that two concurrent runs cannot share, a
//! parked-device probe driven by an independent thread, and a percentile. A copy
//! per file is how the two drift — one file's probe would keep measuring while the
//! other's silently stopped proving anything.

// Each including test target uses a different subset of this harness, so a name
// unused in one is not dead. The lint is real per target and the allowance is
// inherent to sharing one harness across three of them.
#![allow(
    dead_code,
    reason = "each including test target uses a different subset of the shared harness"
)]

use std::error::Error;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

use lgwks_bot::journal::StorageGate;

#[cfg(feature = "script")]
pub use body::{BodyFuture, OneStep, boxed_body, one_step_task};

/// The durable task body the resume files drive. Gated on `script`, so the
/// journal liveness target — which shares only the parked-device instrument —
/// builds with `rt` alone.
#[cfg(feature = "script")]
mod body {
    use std::future::Future;
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
}

/// A unique scratch directory, removed when the guard ends.
pub struct Scratch {
    /// The directory itself.
    path: PathBuf,
    /// The prefix this file's scenarios name theirs by, so a failure leaves a
    /// directory a reader can tell apart from another file's.
    tag: &'static str,
}

impl Scratch {
    /// A scratch directory named by random bytes, so two runs never collide.
    ///
    /// Random bytes rather than a process id or a clock: the OS reuses both, so two
    /// tests in two processes would share a directory and one would delete the
    /// other's store mid-run. The estate's one entropy source is
    /// `lgwks_std::random`, behind this crate's `ephemeral` feature.
    ///
    /// # Errors
    ///
    /// Whatever the entropy source or the filesystem reports.
    pub fn new(tag: &'static str) -> Result<Self, Box<dyn Error>> {
        let unique = lgwks_std::random::bytes::<8>()?;
        let hex: String = unique.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("lgwks-{tag}-{hex}"));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        std::fs::create_dir_all(&path)?;
        Ok(Self { path, tag })
    }

    /// The directory, borrowed.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A path inside the directory.
    #[must_use]
    pub fn join(&self, tail: &str) -> PathBuf {
        self.path.join(tail)
    }

    /// The store file a scenario opens, under this file's own tag.
    #[must_use]
    pub fn store(&self) -> PathBuf {
        self.join(&format!("{}.runstore", self.tag))
    }
}

impl Drop for Scratch {
    /// Remove the directory.
    ///
    /// Best effort: every assertion has already been made, and a leaked temp
    /// directory is a nuisance rather than a wrong answer.
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.path));
    }
}

/// How many turns the unrelated task must get before the watchdog releases the
/// parked device. A blocking durable wait reaches at most one.
pub const PROGRESS_TURNS: u64 = 64;

/// A parked device with the instruments a liveness test needs.
pub struct Parked {
    /// Where the parked append must be attempted, when the caller is parking a
    /// position-fenced write rather than a record append.
    ///
    /// A journal's liveness test needs the fence it is about to contend on; a
    /// record store's does not, because its append takes no caller-supplied
    /// position. One type serves both rather than two that differ by a field,
    /// because the part that is under test is identical: an independent thread
    /// releasing a parked device while an unrelated task keeps turning.
    tail: Option<lgwks_bot::journal::JournalPosition>,
    /// How many turns the unrelated task has taken.
    ticks: Arc<AtomicU64>,
    /// Set by the watchdog once it has released the device.
    released: Arc<AtomicBool>,
    /// The independent thread that releases the device.
    watchdog: std::thread::JoinHandle<u64>,
}

impl Parked {
    /// Park `gate`'s device and start the independent watchdog over it.
    ///
    /// One constructor rather than a copy per test, because the watchdog contract —
    /// an independent thread that releases the device once the unrelated task has
    /// progressed — is the thing under test, and a second copy is a second thing to
    /// drift.
    ///
    /// # Errors
    ///
    /// Whatever the OS reports when the watchdog thread cannot start.
    pub fn new(gate: StorageGate) -> Result<Self, std::io::Error> {
        Self::at(None, gate)
    }

    /// Park a position-fenced write at `tail` as well, for a journal whose
    /// append is refused unless it continues from the right fence.
    ///
    /// # Errors
    ///
    /// Whatever the OS reports when the watchdog thread cannot start.
    pub fn at(
        tail: Option<lgwks_bot::journal::JournalPosition>,
        gate: StorageGate,
    ) -> Result<Self, std::io::Error> {
        let ticks = Arc::new(AtomicU64::new(0));
        let released = Arc::new(AtomicBool::new(false));
        let watchdog = spawn_watchdog(&ticks, gate, Arc::clone(&released))?;
        Ok(Self {
            tail,
            ticks,
            released,
            watchdog,
        })
    }

    /// The fence a parked append must continue from.
    ///
    /// # Errors
    ///
    /// `None` if this `Parked` was opened with [`Self::new`] rather than
    /// [`Self::at`]: a caller asking for a fence it never supplied cannot be
    /// served a value, and a store whose append takes no position has none to
    /// hand back. `Result` rather than a panic because the two kinds of caller
    /// differ by construction and the test must not be the thing that decides
    /// which one it is.
    pub fn tail(&self) -> Result<lgwks_bot::journal::JournalPosition, String> {
        self.tail
            .ok_or_else(|| "this Parked was opened without a fence; use Parked::at".to_owned())
    }

    /// The counter the unrelated task turns.
    #[must_use]
    pub fn ticks(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.ticks)
    }

    /// Whether the watchdog has released the device.
    #[must_use]
    pub fn released(&self) -> &AtomicBool {
        &self.released
    }

    /// Wait for the watchdog and report the turns the runtime achieved.
    ///
    /// # Errors
    ///
    /// When the watchdog thread panicked, which would leave the release unheard.
    pub fn observed(self) -> Result<u64, String> {
        self.watchdog
            .join()
            .map_err(|_| "the watchdog thread panicked".to_owned())
    }
}

/// The unrelated ready task, polled on the same driver as the record: it ticks and
/// re-wakes itself so the driver keeps turning while the flush is parked.
pub fn heartbeat(ticks: Arc<AtomicU64>) -> impl Future<Output = ()> {
    std::future::poll_fn(move |cx| {
        ticks.fetch_add(1, Ordering::Relaxed);
        cx.waker().wake_by_ref();
        Poll::<()>::Pending
    })
}

/// An independent thread that releases the parked device once the unrelated task
/// has demonstrably progressed.
///
/// It is not the runtime and not the storage owner: nothing it does depends on the
/// future under test being polled. It gives up at `deadline` so a tree that is
/// genuinely wedged fails the tick assertion rather than hanging the suite, sets
/// `released` so a later phase can observe the release without asking the store,
/// and reports the tick count it saw.
fn spawn_watchdog(
    ticks: &Arc<AtomicU64>,
    gate: StorageGate,
    released: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<u64>, std::io::Error> {
    let ticks = Arc::clone(ticks);
    std::thread::Builder::new()
        .name("liveness-watchdog".to_owned())
        .spawn(move || {
            let deadline = Instant::now().checked_add(Duration::from_secs(10));
            while deadline.is_none_or(|limit| Instant::now() < limit)
                && ticks.load(Ordering::Relaxed) < PROGRESS_TURNS
            {
                std::thread::park_timeout(Duration::from_millis(1));
            }
            gate.release();
            released.store(true, Ordering::Relaxed);
            ticks.load(Ordering::Relaxed)
        })
}

/// A percentile of a sorted sample, by nearest rank.
///
/// Nearest rank rather than interpolation: the report then names a sample that was
/// actually observed, which is the property a reader of a latency claim needs and
/// an interpolated number does not have.
#[must_use]
pub fn percentile(sorted: &[u128], per_mille: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = sorted.len().saturating_mul(per_mille).div_ceil(1_000);
    sorted[rank.clamp(1, sorted.len()).saturating_sub(1)]
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
    /// The percentiles of `samples`, sorted in place.
    #[must_use]
    pub fn of(samples: &mut [u128]) -> Self {
        samples.sort_unstable();
        Self {
            p50: percentile(samples, 500),
            p95: percentile(samples, 950),
            p99: percentile(samples, 990),
        }
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
        |bytes| {
            format!(
                "{} MiB",
                u128::from(bytes).checked_div(MIB).unwrap_or_default()
            )
        },
    )
}

/// Peak resident set size of this process, or `None` where nothing reports it.
///
/// `/proc/self/status` only, and `None` — never a fabricated number — elsewhere.
/// A machine that cannot answer the question must not have the test invent one.
#[must_use]
pub fn peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kib: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
            return Some(kib.saturating_mul(1024));
        }
    }
    None
}
