//! The bounded record of which steps a run entered, and what overflow cost.
//!
//! A report has to answer "what ran, in what order" without a second ledger
//! running beside the flow. The answer already exists while the flow runs: every
//! [`Scope::enter`] knows the path it is entering.
//! A [`Trail`] is where that path is written down, and it is the only place.
//!
//! # Overflow is a reported state, never a silent loss
//!
//! The ring keeps the **last** `capacity` paths, and every path it drops to make
//! room increments `dropped`. A caller reading a truncated trail can say how
//! much of the run it is not seeing, which is the difference between a bounded
//! observation and a wrong one. What overflow never touches is the terminal
//! state: the disposition, the typed output and the located error come from the
//! run's own value, not from the ring, so a run of ten thousand steps reports
//! its outcome exactly as a run of ten does.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// A bounded, newest-last record of step paths, shared by every scope under one
/// run.
#[derive(Debug)]
pub(crate) struct Trail {
    /// How many paths are retained. One or more: a trail that retains nothing
    /// would answer every question about a run with "no".
    capacity: usize,
    /// The retained paths, oldest first.
    paths: Mutex<VecDeque<Arc<str>>>,
    /// How many paths were dropped to keep the ring at `capacity`.
    dropped: AtomicUsize,
}

impl Trail {
    /// A ring holding at most `capacity` paths.
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity: capacity.max(1),
            paths: Mutex::new(VecDeque::new()),
            dropped: AtomicUsize::new(0),
        })
    }

    /// Take the lock. A poisoned lock still guards a consistent value: the
    /// critical section is a push and a pop with no user code between them, so
    /// an unwind elsewhere is not a reason to lose the trail.
    fn lock(&self) -> MutexGuard<'_, VecDeque<Arc<str>>> {
        crate::journal::owner::lock(&self.paths)
    }

    /// Record that the run entered `path`, dropping the oldest retained path if
    /// the ring is full.
    pub(crate) fn record(&self, path: &Arc<str>) {
        let mut paths = self.lock();
        if paths.len() >= self.capacity {
            // The ring is bounded and the oldest entry is the declared detail
            // this loses; the count is what makes the loss visible.
            paths.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        paths.push_back(Arc::clone(path));
    }

    /// The retained paths, oldest first, and how many were dropped.
    pub(crate) fn snapshot(&self) -> (Vec<Arc<str>>, usize) {
        let paths = self.lock();
        (
            paths.iter().cloned().collect(),
            self.dropped.load(Ordering::Relaxed),
        )
    }
}
