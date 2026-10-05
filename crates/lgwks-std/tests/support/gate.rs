//! A gate every job waits on, shared by the pool's test families.
//!
//! A burst of jobs that finish immediately measures nothing: the threads that
//! run them park again before the next check. A gate holds every job at the
//! start, so a burst is still in flight when the measurement is taken and
//! cannot be over before it is asked for. Both the in-crate lifetime
//! simulation and the process-owning public journey need exactly this, so it
//! lives here once rather than as a copy in each.

// Each test target uses a subset: the lifetime family holds a thread on
// `Release`, the public journey only opens `Gate`.
#![allow(
    dead_code,
    reason = "each test target uses a subset of the hold fixtures"
)]

use std::sync::{Condvar, Mutex, MutexGuard};

/// A latch jobs wait on, opened once for all of them.
pub struct Gate {
    /// Whether the gate is open, under the condvar that waits on it.
    open: Mutex<bool>,
    /// Signalled when the gate opens.
    opened: Condvar,
}

impl Gate {
    /// A gate nobody has opened.
    pub const fn new() -> Self {
        Self {
            open: Mutex::new(false),
            opened: Condvar::new(),
        }
    }

    /// The gate's own state, recovering a poisoned lock: a panic in a job
    /// that held it did not corrupt the flag, and a test that cannot open its
    /// own gate would report that as a hang.
    fn open(&self) -> MutexGuard<'_, bool> {
        self.open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait until the gate opens. A wake with the gate still shut waits again,
    /// so a spurious wakeup is not mistaken for the opening.
    pub fn pass(&self) {
        let mut open = self.open();
        while !*open {
            open = self
                .opened
                .wait(open)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Open the gate, releasing every waiter.
    pub fn release(&self) {
        *self.open() = true;
        self.opened.notify_all();
    }
}

/// A hold one thread at a time can be released from, by moving a generation
/// rather than opening a flag.
///
/// [`Gate`] opens once and stays open, which is right for a burst of jobs and
/// wrong for a fixture a test replays: the second run would find the gate
/// already open and hold nothing. A holder here waits for the generation to
/// pass the one it took, so every run is held by its own release.
pub struct Release {
    /// How many times this has been released.
    generation: Mutex<u64>,
    /// Signalled when the generation moves.
    changed: Condvar,
}

impl Release {
    /// Nothing has been released yet.
    pub const fn new() -> Self {
        Self {
            generation: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    /// The generation a holder takes to be held until this is released again.
    pub fn generation(&self) -> u64 {
        *self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Hold until the generation has moved past `held`.
    pub fn wait_past(&self, held: u64) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *generation <= held {
            generation = self
                .changed
                .wait(generation)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Move the generation on, releasing every holder waiting on this one.
    pub fn release(&self) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *generation = generation.saturating_add(1);
        self.changed.notify_all();
    }
}
