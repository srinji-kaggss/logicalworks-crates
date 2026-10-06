//! A gate every job waits on, shared by the pool's test families.
//!
//! A burst of jobs that finish immediately measures nothing: the threads that
//! run them park again before the next check. A gate holds every job at the
//! start, so a burst is still in flight when the measurement is taken and
//! cannot be over before it is asked for. Both the in-crate lifetime
//! simulation and the process-owning public journey need exactly this, so it
//! lives here once rather than as a copy in each.
//!
//! It carries its own tests rather than an `allow`: the public journey only
//! opens a [`Gate`] and the lifetime family only holds a [`Release`], so each
//! binary leaves half of this file unused, and a fixture whose unused half is
//! silenced is a fixture nobody has read.

use std::sync::{Condvar, Mutex, MutexGuard};

/// The guarded state a lock or a condvar wait hands back, recovering a poisoned
/// lock.
///
/// A panic in a job that held one of these locks left the flag or the counter it
/// guards at the value that job wrote rather than at an arbitrary one: each is
/// updated by a single store under the lock, so the state a later holder reads
/// is the last state some thread wrote and remains a state the type accepts. A
/// test that could not take its own lock would report that as a hang and hide
/// the panic that poisoned it, which is the one fact the reader needs — so the
/// poison is recovered here and the panic stays in the test output.
///
/// [`Mutex::lock`] and [`Condvar::wait`] return the same `LockResult`, which is
/// what lets this be the single place. Every other spelling — a
/// `PoisonError::into_inner` closure at each of the six sites, or an `unwrap` on
/// a poisoned lock — is a second answer to one question, and two answers are
/// how one of them comes to unwrap while the others recover.
fn guard<T>(locked: std::sync::LockResult<MutexGuard<'_, T>>) -> MutexGuard<'_, T> {
    match locked {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

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

    /// Wait until the gate opens. A wake with the gate still shut waits again,
    /// so a spurious wakeup is not mistaken for the opening.
    pub fn pass(&self) {
        let mut open = guard(self.open.lock());
        while !*open {
            open = guard(self.opened.wait(open));
        }
    }

    /// Open the gate, releasing every waiter.
    pub fn release(&self) {
        *guard(self.open.lock()) = true;
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
        *guard(self.generation.lock())
    }

    /// Hold until the generation has moved past `held`.
    pub fn wait_past(&self, held: u64) {
        let mut generation = guard(self.generation.lock());
        while *generation <= held {
            generation = guard(self.changed.wait(generation));
        }
    }

    /// Move the generation on, releasing every holder waiting on this one.
    pub fn release(&self) {
        let mut generation = guard(self.generation.lock());
        *generation = generation.saturating_add(1);
        self.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread::{self, JoinHandle};

    use super::{Gate, Release};

    /// The counter a set of waiters shares, and the handles that join them.
    type Waiters = (Arc<AtomicUsize>, Vec<JoinHandle<()>>);

    /// `waiters` threads, each passing `gate` `passes` times, with the counter
    /// they share and the handles that join them.
    ///
    /// The handles are handed back rather than joined here because the property
    /// under test is a *blocked* pass, and the caller has to release the gate
    /// before the waiters can finish.
    fn waiters(gate: &Arc<Gate>, waiters: usize, passes: usize) -> Result<Waiters, Box<dyn Error>> {
        let passed = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::with_capacity(waiters);
        for _ in 0..waiters {
            let gate = Arc::clone(gate);
            let passed = Arc::clone(&passed);
            handles.push(thread::Builder::new().spawn(move || {
                for _ in 0..passes {
                    gate.pass();
                    passed.fetch_add(1, Ordering::SeqCst);
                }
            })?);
        }
        Ok((passed, handles))
    }

    /// Joins every waiter.
    ///
    /// The join is the observation, not the counter: a `pass` that waited on an
    /// open gate, or one still waiting on a shut one, would leave its thread
    /// blocked and this loop would never finish.
    fn join(handles: Vec<JoinHandle<()>>) -> Result<(), Box<dyn Error>> {
        for handle in handles {
            handle.join().map_err(|_| "a gate waiter panicked")?;
        }
        Ok(())
    }

    #[test]
    /// A shut gate holds every job, and one release frees them all.
    fn sim_a_closed_gate_holds_every_job_until_it_is_released() -> Result<(), Box<dyn Error>> {
        let gate = Arc::new(Gate::new());
        let (passed, handles) = waiters(&gate, 4, 1)?;
        assert_eq!(
            passed.load(Ordering::SeqCst),
            0,
            "a job passed a gate nobody had opened"
        );
        gate.release();
        join(handles)?;
        assert_eq!(
            passed.load(Ordering::SeqCst),
            4,
            "every job must pass a released gate"
        );
        Ok(())
    }

    #[test]
    /// A released gate answers every later waiter, every time it is asked.
    fn sim_later_waiters_pass_an_open_gate_as_many_times_as_they_ask() -> Result<(), Box<dyn Error>>
    {
        let gate = Arc::new(Gate::new());
        gate.release();
        let (passed, handles) = waiters(&gate, 4, 2)?;
        join(handles)?;
        assert_eq!(
            passed.load(Ordering::SeqCst),
            8,
            "four waiters passing an already-open gate twice must return from both calls"
        );
        Ok(())
    }

    #[test]
    /// A holder waits for a generation after its own, not for its own.
    fn sim_a_holder_is_released_by_a_later_generation_and_not_its_own() -> Result<(), Box<dyn Error>>
    {
        let release = Arc::new(Release::new());
        assert_eq!(
            release.generation(),
            0,
            "nothing has been released, so the generation starts at zero"
        );
        // The generation is taken here rather than inside the holder: a holder
        // that took it after both releases would take generation 2 and then be
        // held for ever, which is a fixture racing itself rather than a
        // property of the hold.
        let held = release.generation();
        let holder = Arc::clone(&release);
        let handles = vec![thread::Builder::new().spawn(move || {
            holder.wait_past(held);
            held
        })?];
        assert_eq!(
            release.generation(),
            0,
            "a holder taking a generation does not move it"
        );
        release.release();
        release.release();
        let released_at = handles
            .into_iter()
            .next()
            .ok_or("the holder thread was not spawned")?
            .join()
            .map_err(|_| "a holder panicked")?;
        assert_eq!(
            released_at, held,
            "the holder must report the generation it took, not the one that released it"
        );
        assert_eq!(
            release.generation(),
            2,
            "two releases moved the generation twice"
        );
        Ok(())
    }

    #[test]
    /// A holder whose generation already moved returns without waiting.
    fn a_holder_already_past_its_generation_does_not_wait() {
        let release = Release::new();
        release.release();
        release.wait_past(0);
        assert_eq!(
            release.generation(),
            1,
            "the generation moved once, and the holder took zero"
        );
    }
}
