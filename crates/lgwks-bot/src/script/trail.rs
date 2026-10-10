//! The bounded record of which steps a run entered, the script line each came
//! from, and how each ended.
//!
//! A report has to answer "what ran, in what order" without a second ledger
//! running beside the flow. The answer already exists while the flow runs: every
//! [`Scope::enter`] knows the path it is entering, and a step `script!` wrote
//! also knows the line it was written on.
//! A [`Trail`] is where that is written down, and it is the only place.
//!
//! # A run reads back as its script (SL-6)
//!
//! Each entry carries the [`Site`] of the line that produced it, when a
//! `script!` line did, and the [`Outcome`] the step settled with. A finished
//! run therefore renders as the script's own lines, in the order they ran, each
//! with what happened to it ([`ScriptTrail`]). The site rides beside the step
//! key and never inside it: keys are structural, so a resumed run replays across
//! a deploy that moved every line.
//!
//! # Overflow is a reported state, never a silent loss
//!
//! The ring keeps the **last** `capacity` entries, and every entry it drops to
//! make room increments `dropped`. A caller reading a truncated trail can say
//! how much of the run it is not seeing, which is the difference between a
//! bounded observation and a wrong one. What overflow never touches is the
//! terminal state: the disposition, the typed output and the located error come
//! from the run's own value, not from the ring, so a run of ten thousand steps
//! reports its outcome exactly as a run of ten does.
//!
//! [`Scope::enter`]: crate::script::Scope::enter

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use super::{FlowError, StepKind};

// ── Site ────────────────────────────────────────────────────────────────────

/// Where a step stands in its script: the word, the source line, and the line
/// as written.
///
/// `script!` emits one `static` per line that enters a scope, so a step costs
/// its trail a pointer, not a copy of its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Site {
    /// The word the line starts with.
    kind: StepKind,
    /// The 1-based source line, as the compiler reports it.
    line: u32,
    /// The line as written, without its `:`.
    text: &'static str,
}

impl Site {
    #[doc(hidden)]
    /// Describe a line. Called by `script!`.
    #[must_use]
    pub const fn new(kind: StepKind, line: u32, text: &'static str) -> Self {
        Self { kind, line, text }
    }

    /// The word the line starts with.
    #[must_use]
    pub const fn kind(&self) -> StepKind {
        self.kind
    }

    /// The 1-based source line, as the compiler reports it.
    #[must_use]
    pub const fn line(&self) -> u32 {
        self.line
    }

    /// The line as written, without its `:`.
    #[must_use]
    pub const fn text(&self) -> &'static str {
        self.text
    }
}

// ── Outcome ─────────────────────────────────────────────────────────────────

/// How one entered step ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// It gave back a value on its first try.
    Succeeded,
    /// A `retry` gave back a value on attempt `attempts`, after `attempts - 1`
    /// failed ones.
    Retried {
        /// The attempt that succeeded, counting the first.
        attempts: u32,
    },
    /// A deadline passed before the step finished.
    TimedOut {
        /// The budget the `within` line declared.
        after: Duration,
    },
    /// The step, or one above it, was stopped.
    Cancelled,
    /// The step failed.
    Failed {
        /// The located failure, as it renders.
        reason: Arc<str>,
    },
}

impl Outcome {
    /// The outcome `result` describes.
    pub(crate) fn of<T>(result: &Result<T, FlowError>) -> Self {
        match *result {
            Ok(_) => Self::Succeeded,
            Err(FlowError::TimedOut { after, .. }) => Self::TimedOut { after },
            Err(ref error) if error.is_cancelled() => Self::Cancelled,
            Err(ref error) => Self::Failed {
                reason: Arc::from(error.to_string()),
            },
        }
    }

    /// The outcome of a `retry` that made `attempts` attempts and ended with
    /// `result`.
    pub(crate) fn of_attempts<T>(result: &Result<T, FlowError>, attempts: u32) -> Self {
        match *result {
            Ok(_) if attempts > 1 => Self::Retried { attempts },
            _ => Self::of(result),
        }
    }
}

impl fmt::Display for Outcome {
    /// `ok`, `retried N, then ok`, `timed out after D`, `stopped`, or
    /// `failed with <reason>`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Succeeded => formatter.write_str("ok"),
            Self::Retried { attempts } => {
                write!(formatter, "retried {}, then ok", attempts.saturating_sub(1))
            }
            Self::TimedOut { after } => write!(formatter, "timed out after {after:?}"),
            Self::Cancelled => formatter.write_str("stopped"),
            Self::Failed { ref reason } => write!(formatter, "failed with {reason}"),
        }
    }
}

// ── Marks and entries ───────────────────────────────────────────────────────

/// One entered step while the run is live: shared by the ring and the scope
/// that entered it, so the scope can settle it without finding it again.
#[derive(Debug)]
pub(crate) struct Mark {
    /// The step's path.
    path: Arc<str>,
    /// The line that produced it, when `script!` wrote it.
    site: Option<&'static Site>,
    /// How many scopes lie between the root and the step.
    depth: u16,
    /// How it ended, once it has.
    outcome: OnceLock<Outcome>,
}

impl Mark {
    /// The step's path.
    pub(crate) const fn path(&self) -> &Arc<str> {
        &self.path
    }

    /// The line that produced the step, when `script!` wrote it.
    pub(crate) const fn site(&self) -> Option<&'static Site> {
        self.site
    }

    /// Record how the step ended. The first settlement stands; `outcome` is
    /// not evaluated after it, so a failure's text is rendered at most once.
    pub(crate) fn settle(&self, outcome: impl FnOnce() -> Outcome) {
        self.outcome.get_or_init(outcome);
    }

    /// The entry a report keeps.
    fn entry(&self) -> TrailEntry {
        TrailEntry {
            path: Arc::clone(&self.path),
            site: self.site,
            depth: self.depth,
            outcome: self.outcome.get().cloned(),
        }
    }
}

/// One step a finished run entered: its path, its script line and its outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailEntry {
    /// The step's path.
    path: Arc<str>,
    /// The line that produced it, when `script!` wrote it.
    site: Option<&'static Site>,
    /// How many scopes lie between the root and the step.
    depth: u16,
    /// How it ended; `None` when it never settled.
    outcome: Option<Outcome>,
}

impl TrailEntry {
    /// The step's path, as [`crate::task::Report::steps`] lists it.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The script line that produced the step; `None` for a step entered by
    /// hand rather than written in `script!`.
    #[must_use]
    pub const fn site(&self) -> Option<&'static Site> {
        self.site
    }

    /// How many scopes lie between the root and the step.
    #[must_use]
    pub const fn depth(&self) -> u16 {
        self.depth
    }

    /// How the step ended.
    ///
    /// `None` when it never settled: it was left by an early exit from a `for`
    /// body, or the run ended while it was still in flight. The step that did
    /// settle above it says why.
    #[must_use]
    pub const fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    /// The shared path, for the report's path list.
    pub(crate) fn shared_path(&self) -> Arc<str> {
        Arc::clone(&self.path)
    }
}

/// A finished run's trail rendered as its script: one line per entered step,
/// in the order the steps were entered.
///
/// ```text
///   14 | flow lines(passes: u64) -> u64  -> ok
///   16 |   for pass in 0..passes #0  -> ok
///   17 |     step pass_step  -> ok
/// ```
///
/// Each line is the source line number, the step's line as written, indented
/// by nesting, an item number for one body of an `each` or `for`, and the
/// outcome. A step entered by hand shows its path in place of a line. A
/// truncated trail says first how many earlier steps it does not show.
#[derive(Debug, Clone, Copy)]
pub struct ScriptTrail<'report> {
    /// The retained entries, oldest first.
    entries: &'report [TrailEntry],
    /// How many entries the ring dropped.
    dropped: usize,
}

impl<'report> ScriptTrail<'report> {
    /// A rendering of `entries`, after `dropped` entries the ring did not keep.
    pub(crate) const fn new(entries: &'report [TrailEntry], dropped: usize) -> Self {
        Self { entries, dropped }
    }
}

impl fmt::Display for ScriptTrail<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.dropped > 0 {
            writeln!(
                formatter,
                "     | ({} earlier steps not kept)",
                self.dropped
            )?;
        }
        // Indented from the shallowest entry kept, so a truncated trail still
        // starts at the left margin. An empty trail renders no line, so the
        // fold's start never reaches the output.
        let shallowest = self
            .entries
            .iter()
            .map(|entry| entry.depth)
            .fold(u16::MAX, u16::min);
        for entry in self.entries {
            match entry.site {
                Some(site) => write!(formatter, "{:>4} | ", site.line)?,
                None => formatter.write_str("   - | ")?,
            }
            for _ in shallowest..entry.depth {
                formatter.write_str("  ")?;
            }
            match entry.site {
                Some(site) => formatter.write_str(site.text)?,
                None => formatter.write_str(&entry.path)?,
            }
            // One body of an `each` or `for` shares its header's line; the
            // item number after the path's last `#` tells the bodies apart.
            let last_segment = entry.path.rsplit('/').next();
            if entry.site.is_some()
                && let Some((_, item)) = last_segment.and_then(|segment| segment.rsplit_once('#'))
            {
                write!(formatter, " #{item}")?;
            }
            match entry.outcome {
                Some(ref outcome) => writeln!(formatter, "  -> {outcome}")?,
                None => writeln!(formatter, "  -> did not finish")?,
            }
        }
        Ok(())
    }
}

// ── Trail ───────────────────────────────────────────────────────────────────

/// A bounded, newest-last record of entered steps, shared by every scope under
/// one run.
#[derive(Debug)]
pub(crate) struct Trail {
    /// How many entries are retained. One or more: a trail that retains nothing
    /// would answer every question about a run with "no".
    capacity: usize,
    /// The retained entries, oldest first.
    marks: Mutex<VecDeque<Arc<Mark>>>,
    /// How many entries were dropped to keep the ring at `capacity`.
    dropped: AtomicUsize,
}

impl Trail {
    /// A ring holding at most `capacity` entries.
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity: capacity.max(1),
            marks: Mutex::new(VecDeque::new()),
            dropped: AtomicUsize::new(0),
        })
    }

    /// Take the lock. A poisoned lock still guards a consistent value: the
    /// critical section is a push and a pop with no user code between them, so
    /// an unwind elsewhere is not a reason to lose the trail.
    fn lock(&self) -> MutexGuard<'_, VecDeque<Arc<Mark>>> {
        crate::journal::owner::lock(&self.marks)
    }

    /// Record that the run entered `path` from `site` at `depth`, dropping the
    /// oldest retained entry if the ring is full. The mark is returned so the
    /// step can settle it; a mark the ring has dropped still settles, and no
    /// one reads it.
    pub(crate) fn record(
        &self,
        path: &Arc<str>,
        site: Option<&'static Site>,
        depth: u16,
    ) -> Arc<Mark> {
        let mark = Arc::new(Mark {
            path: Arc::clone(path),
            site,
            depth,
            outcome: OnceLock::new(),
        });
        let mut marks = self.lock();
        if marks.len() >= self.capacity {
            // The ring is bounded and the oldest entry is the declared detail
            // this loses; the count is what makes the loss visible.
            marks.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        marks.push_back(Arc::clone(&mark));
        mark
    }

    /// The retained entries, oldest first, and how many were dropped.
    pub(crate) fn snapshot(&self) -> (Vec<TrailEntry>, usize) {
        let marks = self.lock();
        (
            marks.iter().map(|mark| mark.entry()).collect(),
            self.dropped.load(Ordering::Relaxed),
        )
    }
}
