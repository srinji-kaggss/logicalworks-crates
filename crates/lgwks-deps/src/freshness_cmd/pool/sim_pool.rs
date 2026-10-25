//! The pool against a rendezvous gate that a serial client cannot open, and
//! against seeded sweeps of its bound, its order and its fault isolation.

use std::error::Error;
use std::panic::resume_unwind;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use lgwks_std::seeded::Seeded;

use super::{PoolError, run, workers_for};

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// A gate that opens once `target` jobs are inside it at the same time.
///
/// Once open it stays open, so later jobs pass. A job that waits `SAFETY`
/// without the gate opening reports that it did not: the only way that
/// happens is a pool too serial to bring `target` jobs in together, and it
/// is a failed assertion rather than a hung test. No assertion measures a
/// duration; the timeout only bounds a failure.
struct Gate {
    /// Jobs that have arrived, and whether the gate has opened.
    state: Mutex<(usize, bool)>,
    /// Wakes the waiters when the gate opens.
    opened: Condvar,
    /// How many jobs must be inside together.
    target: usize,
    /// Jobs inside right now.
    inside: AtomicUsize,
    /// The most jobs that were inside at once.
    peak: AtomicUsize,
}

/// The longest a job waits for the gate before it reports a failure.
const SAFETY: Duration = Duration::from_secs(20);

impl Gate {
    /// A gate of `target` jobs.
    fn new(target: usize) -> Self {
        Self {
            state: Mutex::new((0, false)),
            opened: Condvar::new(),
            target,
            inside: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    /// Enters, waits for the gate, and leaves; true when it opened in time.
    fn pass(&self) -> bool {
        let now = self.inside.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        self.peak.fetch_max(now, Ordering::SeqCst);
        let mut guard = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.0 = guard.0.saturating_add(1);
        if guard.0 >= self.target {
            guard.1 = true;
            self.opened.notify_all();
        }
        let open = match self
            .opened
            .wait_timeout_while(guard, SAFETY, |gate_state| !gate_state.1)
        {
            Ok((waited, _timeout)) => waited.1,
            Err(poisoned) => poisoned.into_inner().0.1,
        };
        self.inside.fetch_sub(1, Ordering::SeqCst);
        open
    }
}

#[test]
fn the_worker_count_is_the_smaller_of_the_bound_and_the_work() {
    for limit in 1..=40_usize {
        for items in 0..=70_usize {
            assert_eq!(
                workers_for(limit, items),
                limit.min(items),
                "limit {limit}, items {items}"
            );
        }
    }
    assert_eq!(workers_for(32, 0), 0, "no work needs no worker");
    assert_eq!(workers_for(32, 212), 32, "212 packages run 32 at a time");
    assert_eq!(workers_for(1, 212), 1, "a bound of one is serial");
}

#[test]
fn a_pool_brings_its_whole_bound_in_flight_together_and_never_more() {
    for (limit, items) in [
        (1_usize, 5_usize),
        (4, 4),
        (5, 20),
        (8, 3),
        (32, 212),
        (16, 16),
    ] {
        let want = workers_for(limit, items);
        let gate = Arc::new(Gate::new(want));
        let job_gate = Arc::clone(&gate);
        let done = run((0..items).collect::<Vec<_>>(), limit, move |_item| {
            job_gate.pass()
        });
        assert_eq!(done.workers, want, "limit {limit}, items {items}");
        assert_eq!(done.results.len(), items, "one result per item");
        for outcome in &done.results {
            assert_eq!(
                outcome,
                &Ok(true),
                "limit {limit}, items {items}: the gate never opened, so the pool is serial"
            );
        }
        assert_eq!(
            gate.peak.load(Ordering::SeqCst),
            want,
            "limit {limit}, items {items}: the most in flight is exactly the bound"
        );
    }
}

#[test]
fn results_come_back_in_input_order_whatever_the_bound() -> TestResult {
    for seed in 0..60_u64 {
        let mut stream = Seeded::from_seed(seed);
        let count = stream.below(90)?;
        let mut items = Vec::new();
        for _ in 0..count {
            items.push(stream.below(1_000)?);
        }
        let expected: Vec<Result<u64, PoolError>> = items
            .iter()
            .map(|item| Ok(item.saturating_mul(3)))
            .collect();
        for limit in [1_usize, 2, 3, 7, 32, 64] {
            let done = run(items.clone(), limit, |item| item.saturating_mul(3));
            assert_eq!(
                done.results, expected,
                "seed {seed}, limit {limit}: order or value changed"
            );
        }
    }
    Ok(())
}

#[test]
fn a_panic_costs_its_own_item_and_no_other() -> TestResult {
    for seed in 0..40_u64 {
        let mut stream = Seeded::from_seed(seed);
        let count = stream.below(40)?.saturating_add(1);
        let mut items = Vec::new();
        for position in 0..count {
            items.push((position, stream.below(4)? == 0));
        }
        let done = run(items.clone(), 5, |(position, faulty)| {
            if faulty {
                // Resumed rather than raised: it unwinds as a panic does,
                // without the default hook printing a message per item.
                resume_unwind(Box::new("seeded lookup failure"));
            }
            position
        });
        assert_eq!(done.results.len(), items.len(), "seed {seed}: a hole");
        for (item, outcome) in items.iter().zip(&done.results) {
            let expected = if item.1 {
                Err(PoolError::Panicked)
            } else {
                Ok(item.0)
            };
            assert_eq!(outcome, &expected, "seed {seed}: item {}", item.0);
        }
    }
    Ok(())
}

#[test]
fn an_empty_run_starts_no_worker() {
    let done = run(Vec::<u8>::new(), 32, |item| item);
    assert!(done.results.is_empty(), "no items, no results");
    assert_eq!(done.workers, 0, "no items, no workers");
}
