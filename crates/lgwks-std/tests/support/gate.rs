//! A gate every job waits on, shared by the pool's test families.
//!
//! A burst of jobs that finish immediately measures nothing: the threads that
//! run them park again before the next check. A gate holds every job at the
//! start, so a burst is still in flight when the measurement is taken and
//! cannot be over before it is asked for. Both the in-crate lifetime
//! simulation and the process-owning public journey need exactly this, so it
//! lives here once rather than as a copy in each.

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
