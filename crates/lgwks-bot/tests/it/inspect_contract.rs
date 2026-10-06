//! Black-box acceptance for the supervisor's inspection surface.
//!
//! The issue this covers asks for bounded, queryable snapshots from the same
//! owner/reducer that runs the work — not a second ledger, and not a
//! high-cardinality log. These tests drive the public API only, and assert the
//! three properties that make the surface trustworthy:
//!
//! 1. **It is the owner.** What the snapshot reports and what the supervisor
//!    admits are the same fact read twice, so a caller that consults the snapshot
//!    before spawning never learns "there is room" from a supervisor that then
//!    refuses.
//! 2. **It is bounded.** The live listing is capped at the in-flight ceiling and
//!    says when it truncated; nothing accumulates between calls.
//! 3. **It terminates.** Under cancellation a snapshot says admission is closed,
//!    and the terminal record stays obtainable through the report stream.

#![cfg(all(feature = "rt", feature = "sync", feature = "time"))]

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::supervise::{NextAction, Supervisor, TaskOutcome, TaskState};
use lgwks_bot::rt::sync::Semaphore;
use lgwks_bot::rt::time::sleep;

/// A gate a body blocks on, so a test can observe a *running* task rather than
/// a finished one.
///
/// The alternative — a `sleep` — makes the test's assertion a statement about
/// wall-clock timing, which is the failure mode this whole issue is about. A
/// semaphore the test itself opens is an observation, not a race.
///
/// The semaphore, rather than a `Notify`, because a notification sent before the
/// waiter registered is **lost**: the body would hang past the end of the test,
/// which is a worse failure than a wrong count because it produces no assertion
/// at all. A permit added to a semaphore is stored and taken by the next
/// acquirer, so a body that arrives before or after the release behaves the same.
#[derive(Clone)]
struct Gate {
    /// Zero permits until the test opens it. An `Arc` so the clone a body captures
    /// is the same gate, not a copy of its count.
    permits: Arc<Semaphore>,
    entered: Arc<AtomicU64>,
    exited: Arc<AtomicU64>,
}

impl Gate {
    /// A closed gate that has admitted nobody.
    fn closed() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(0)),
            entered: Arc::new(AtomicU64::new(0)),
            exited: Arc::new(AtomicU64::new(0)),
        }
    }

    /// An already-open gate, so a body runs straight through.
    ///
    /// A large but *legal* permit count rather than `usize::MAX`: the engine's
    /// semaphore caps at `MAX_PERMITS`, and exceeding it panics inside the
    /// engine — a failure in a helper rather than in the thing under test. The
    /// count is far above any body count in this file, so every acquirer is
    /// admitted immediately and none blocks.
    fn open() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(1_000)),
            entered: Arc::new(AtomicU64::new(0)),
            exited: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Announce arrival, wait for the release, then record the departure.
    ///
    /// The exit counter exists because the supervisor's own `completed` only
    /// advances when a task is **reaped**, and a reaping reap is driven only by a
    /// spawn or an explicit [`Supervisor::reap`] call. A test waiting for a body
    /// to *finish* therefore cannot watch the supervisor's counter and wait for
    /// the same moment — it would be waiting for the very reap it is waiting on.
    /// A body counting its own exit is the observation that does not depend on
    /// the supervisor doing anything at all.
    async fn arrive_and_wait(&self) {
        // The announcement precedes the await, so a test that waits for
        // `arrived` knows every counted body has at least begun.
        self.entered.fetch_add(1, Ordering::SeqCst);
        let Ok(permit) = self.permits.acquire().await else {
            return;
        };
        permit.forget();
        self.exited.fetch_add(1, Ordering::SeqCst);
    }

    /// Let `count` waiters through.
    fn release(&self, count: usize) {
        self.permits.add_permits(count);
    }

    /// How many bodies have arrived.
    fn arrived(&self) -> u64 {
        self.entered.load(Ordering::SeqCst)
    }

    /// How many bodies have finished.
    fn exited(&self) -> u64 {
        self.exited.load(Ordering::SeqCst)
    }

    /// Wait until `expected` bodies have finished.
    ///
    /// The exit-side counterpart of [`Gate::await_arrivals`], bounded the same
    /// way and for the same reason.
    async fn await_exits(&self, expected: u64) {
        let started = std::time::Instant::now();
        while self.exited() < expected {
            assert!(
                within_patience(started),
                "only {} of {expected} bodies finished within {PATIENCE:?}",
                self.exited()
            );
            sleep(Duration::from_millis(1)).await;
        }
    }

    /// Wait until `expected` bodies have announced themselves.
    ///
    /// Bounded by [`PATIENCE`] rather than by a spin count, because the wait has
    /// to give a *different worker thread* the opportunity to run the body:
    /// `yield_now` reschedules only the yielding task, so a loop of them never
    /// lets the task under observation run at all. A real timer does.
    async fn await_arrivals(&self, expected: u64) {
        let started = std::time::Instant::now();
        while self.arrived() < expected {
            assert!(
                within_patience(started),
                "only {} of {expected} bodies announced themselves within {PATIENCE:?}",
                self.arrived()
            );
            sleep(Duration::from_millis(1)).await;
        }
    }
}

/// How long a bounded wait in this file may run before it reports a failure.
///
/// The only wall-clock constant in the file, and it bounds a *wait*, never an
/// assertion: every assertion below is an exact equality over an observed fact.
/// A test that cannot reach its state in this long has a real defect, and
/// failing to say so is the alternative.
const PATIENCE: Duration = Duration::from_secs(5);

/// Whether a wait that started at `started` is still inside its patience budget.
///
/// Measured as elapsed time rather than as a deadline computed by addition: an
/// `Instant` far from its own origin cannot carry [`PATIENCE`], and a stand-in
/// instant for that case would turn a bounded wait into an unbounded one, which
/// is the opposite of what this helper exists to prevent.
fn within_patience(started: std::time::Instant) -> bool {
    match started.elapsed().checked_sub(PATIENCE) {
        // Under the budget, or exactly on it: a `Duration` cannot represent
        // "longer than this", so the subtraction is the total comparison.
        None => true,
        Some(over) => over.is_zero(),
    }
}

/// Wait until `condition` reads true, or fail.
///
/// The same two properties as [`Gate::await_arrivals`]: bounded by [`PATIENCE`],
/// and never itself the assertion. It exists because several tests wait for a
/// supervisor's *counters* rather than for its bodies, and a copy of the loop at
/// each call site would drift.
async fn await_until(mut condition: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !condition() {
        assert!(
            within_patience(started),
            "the awaited condition never became true within {PATIENCE:?}"
        );
        sleep(Duration::from_millis(1)).await;
    }
}

/// Reap until `expected` tasks are counted completed, or fail.
///
/// A body counting its own exit has returned, but its task's terminal state is
/// read by the wrapper *after* that — from the token — and is absorbed only when
/// a reap finds it join-ready. A test that called `shutdown` straight after
/// `await_exits` would race both: the cancel `shutdown` issues first can land
/// between the body's return and the wrapper's read, turning a success into a
/// cancellation. Reaping every outcome first settles each task's state before
/// anything is cancelled.
async fn reap_until_completed(supervisor: &mut Supervisor, expected: u64) {
    await_until(|| {
        supervisor.reap();
        supervisor.stats().completed >= expected
    })
    .await;
}

/// A fresh supervisor of `bound` in the runtime under test.
fn supervisor(bound: usize) -> Result<(Runtime, Supervisor), Box<dyn Error>> {
    let runtime = Runtime::new()?;
    Ok((runtime, Supervisor::new(bound)))
}

/// Place `count` gated bodies on `supervisor` and wait until all of them have
/// announced themselves.
///
/// One definition rather than a loop copied into each test: the sequence is
/// three steps that must happen in this order — spawn, await the arrivals, and
/// only then observe — and a copy that skipped the middle step would be asserting
/// about a supervisor whose tasks had not run at all. The caller holds the
/// [`Gate`] and releases it when its observation is done.
async fn spawn_gated(supervisor: &mut Supervisor, gate: &Gate, count: u64) {
    for _ in 0..count {
        let body = gate.clone();
        supervisor
            .spawn(move |_token| async move { body.arrive_and_wait().await })
            .await;
    }
    gate.await_arrivals(count).await;
}

/// A fresh supervisor is empty, at its declared ceiling, and admits.
#[test]
fn a_fresh_snapshot_admits_at_the_declared_ceiling() -> Result<(), Box<dyn Error>> {
    let (_runtime, supervisor) = supervisor(4)?;
    let snapshot = supervisor.snapshot();
    assert_eq!(
        snapshot.max_in_flight(),
        4,
        "the snapshot reports the ceiling the caller asked for"
    );
    assert!(snapshot.live().is_empty(), "nothing has been spawned");
    assert!(snapshot.is_empty(), "an empty supervisor reads as empty");
    assert_eq!(
        snapshot.free(),
        4,
        "every permit is free before the first spawn"
    );
    assert_eq!(
        snapshot.next_action(),
        NextAction::Admit,
        "a supervisor at capacity zero is the only one that admits"
    );
    assert!(
        !snapshot.is_cancelled(),
        "a fresh supervisor has not been stopped"
    );
    Ok(())
}

/// The snapshot's live listing and the supervisor's own admission agree: a
/// caller that consults `next_action` and then spawns is never told there is
/// room and refused. This is the property that makes it the owner's view rather
/// than an estimate.
#[test]
fn the_snapshot_and_the_admission_decision_agree() -> Result<(), Box<dyn Error>> {
    let (runtime, mut supervisor) = supervisor(2)?;
    runtime.block_on(async {
        let gate = Gate::closed();
        spawn_gated(&mut supervisor, &gate, 2).await;
        // Both tasks are placed and both hold a permit. Waiting for the bodies
        // to announce themselves is what makes the assertion below an
        // observation rather than a race: until a body has run at all, its
        // permit is held but nothing proves the task is live.
        let snapshot = supervisor.snapshot();
        assert_eq!(
            snapshot.occupied(),
            2,
            "two spawned bodies hold both permits, and the snapshot reads the \\
             same two permits the semaphore does"
        );
        assert_eq!(snapshot.free(), 0, "no permit is free at the ceiling");
        assert_eq!(
            snapshot.next_action(),
            NextAction::WaitForCapacity,
            "a full supervisor waits, it does not admit and it does not stop"
        );
        assert_eq!(snapshot.live().len(), 2, "both live tasks are listed");
        assert_eq!(
            snapshot.live_truncated(),
            0,
            "two tasks fit inside a ceiling of two, so nothing was truncated"
        );

        // A third spawn at capacity is what `next_action` predicted.
        let refused = supervisor.try_spawn(|_token| async {});
        assert_eq!(
            refused,
            Err(lgwks_bot::rt::supervise::TrySpawnRefusal::AtCapacity),
            "the snapshot said wait-for-capacity and the supervisor refused, so \\
             the two are the same fact"
        );

        gate.release(2);
        // Wait for both bodies to finish before draining, so the drain reports
        // two completions rather than two cancellations: `shutdown` cancels its
        // token first, and a body that has not yet observed the released permit
        // is reported as cancelled even though its work finished.
        gate.await_exits(2).await;
        reap_until_completed(&mut supervisor, 2).await;
        let report = supervisor.shutdown().await;
        assert_eq!(
            report.stats().succeeded,
            2,
            "both released bodies completed and are counted"
        );
        assert!(
            report.is_clean(),
            "a clean release reports a clean drain, and the panic list is empty"
        );
    });
    Ok(())
}

/// The live listing is bounded by the ceiling and says when it truncated. A
/// caller polling capacity must never be handed an unbounded `Vec`.
#[test]
fn the_live_listing_is_bounded_by_the_ceiling_and_says_it_truncated() -> Result<(), Box<dyn Error>>
{
    let (runtime, mut supervisor) = supervisor(3)?;
    runtime.block_on(async {
        let gate = Gate::closed();
        spawn_gated(&mut supervisor, &gate, 3).await;
        let snapshot = supervisor.snapshot();
        assert_eq!(
            snapshot.live_limit(),
            3,
            "the listing's own bound is the ceiling"
        );
        assert!(
            snapshot.live().len() <= snapshot.live_limit(),
            "the listing never exceeds its declared ceiling: {} > {}",
            snapshot.live().len(),
            snapshot.live_limit()
        );
        assert_eq!(
            snapshot.live_truncated(),
            0,
            "three tasks fit a ceiling of three"
        );

        // Each entry carries the supervisor's own identity, in spawn order, so a
        // terminal report can be matched to a live listing without a second
        // registry.
        for (index, live) in snapshot.live().iter().enumerate() {
            assert_eq!(
                live.task.get(),
                u64::try_from(index)?,
                "the listing is in spawn order, so entry {index} is the task \\
                 placed at that position"
            );
            assert_eq!(
                live.state,
                TaskState::Running { position: index },
                "each live entry names its position in spawn order"
            );
            assert!(live.state.is_running(), "a gated body has not finished");
        }

        gate.release(3);
        gate.await_exits(3).await;
        reap_until_completed(&mut supervisor, 3).await;
        let report = supervisor.shutdown().await;
        assert_eq!(
            report.stats().succeeded,
            3,
            "all three released bodies completed"
        );
        Ok::<_, Box<dyn Error>>(())
    })?;
    Ok(())
}

/// After cancellation the snapshot says admission is closed, and — crucially —
/// the terminal record is still obtainable through the report stream. A
/// snapshot that swallowed the terminal evidence would be an inspection surface
/// that only works while everything is fine.
#[test]
fn cancellation_closes_admission_and_the_terminal_record_stays_readable()
-> Result<(), Box<dyn Error>> {
    let (runtime, mut supervisor) = supervisor(2)?;
    runtime.block_on(async {
        let gate = Gate::closed();
        spawn_gated(&mut supervisor, &gate, 2).await;
        supervisor.cancel();
        let snapshot = supervisor.snapshot();
        assert!(
            snapshot.is_cancelled(),
            "a cancelled supervisor reads as cancelled"
        );
        assert_eq!(
            snapshot.next_action(),
            NextAction::Stopped,
            "a cancelled supervisor admits nothing; a spawn here would be refused \\
             rather than queued, so waiting for capacity would wait forever"
        );
        assert_eq!(
            supervisor.try_spawn(|_token| async {}),
            Err(lgwks_bot::rt::supervise::TrySpawnRefusal::Cancelled),
            "the refusal names the same world the snapshot's next_action did"
        );

        gate.release(2);
        let report = supervisor.shutdown().await;
        // The terminal record survives cancellation and stays attributable.
        assert!(
            report.stats().succeeded + report.stats().cancelled >= 2,
            "both released bodies are counted in one of the two terminal \\
             counters, not lost"
        );
        let outcomes = report.outcomes();
        assert_eq!(
            outcomes.len(),
            2,
            "every drained task has exactly one terminal outcome"
        );
        let mut ids: Vec<u64> = outcomes
            .iter()
            .map(|outcome| outcome.task().get())
            .collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![0, 1],
            "the two identities are distinct and neither aliased the other"
        );
    });
    Ok(())
}

/// A caller that never drains its reports loses detail, never memory, and both
/// halves of that statement are asserted here: the dropped-detail counter is
/// non-zero, and every task is still accounted as finished. The retained
/// outcomes remain obtainable through the stream, and the list holds exactly the
/// tasks the cap kept — no more, no fewer.
#[test]
fn an_undrained_supervisor_reports_dropped_detail_without_growing() -> Result<(), Box<dyn Error>> {
    let (runtime, mut supervisor) = supervisor(2)?;
    runtime.block_on(async {
        // Six quick bodies into a ceiling of two. The bodies count their own exits so
        // the test can wait for the work to be genuinely done: `shutdown`
        // cancels its token first, so a body that has not yet run is reported
        // cancelled, and the test would be measuring the shutdown's timing
        // rather than the retention it is about.
        let gate = Gate::open();
        spawn_gated(&mut supervisor, &gate, 6).await;
        // Six bodies into a ceiling of two. Every spawn reaps first, and that reap
        // uses the *capped* retention, so outcomes produced between spawns are
        // dropped once the report buffer of two is full. The drain itself is
        // unbounded, but it can only report what was retained. Both facts are
        // asserted: the counter says what was lost, and the outcome list says
        // what survived. A caller that read either alone would be guessing.
        gate.await_exits(6).await;
        // A body counting its exit is not yet a task the join set can hand back,
        // and `shutdown` absorbs whatever is left *uncapped*. Left to the
        // scheduler, a runner where no body was join-ready at any spawn would
        // drain all six at shutdown and drop nothing — and a body whose wrapper
        // had not yet read its token when `shutdown` cancelled it would be
        // reported cancelled rather than succeeded. Every outcome is therefore
        // absorbed here, through the capped reap an undrained caller gets, so the
        // loss is a fact of the cap and not of the runner's timing.
        reap_until_completed(&mut supervisor, 6).await;
        let report = supervisor.shutdown().await;
        let stats = report.stats();
        assert_eq!(stats.spawned, 6, "all six bodies were placed");
        assert_eq!(
            stats.succeeded, 6,
            "all six completed and are counted as such"
        );
        assert_eq!(
            stats.spawned, stats.completed,
            "the accounting total adds up: six spawned, six finished"
        );
        assert_eq!(
            stats.reports_dropped, 4,
            "a caller that spawned six bodies into a report buffer of two \\
             without draining it lost exactly the four outcomes past the cap; \\
             the counter says so rather than the loss being silent"
        );
        assert!(
            stats.reports_dropped < stats.completed,
            "the cap drops detail, not outcomes: all {} tasks were still \\
             accounted as finished",
            stats.completed
        );
        let mut outcomes = report.into_outcomes().map_err(|refused| {
            format!(
                "a shutdown report with no retained cleanup owner refused to convert \
                 to exactly its outcomes: {refused:?}"
            )
        })?;
        outcomes.sort_by_key(TaskOutcome::task);
        assert_eq!(
            outcomes.len(),
            usize::try_from(stats.completed - stats.reports_dropped)?,
            "the terminal record holds every outcome the retention cap kept, \\
             and no more"
        );
        Ok::<_, Box<dyn Error>>(())
    })?;
    Ok(())
}

/// Repeating reads do not accumulate. Polling a snapshot in a loop is the normal
/// way an owner watches capacity, and a surface that grew on every read would
/// make the loop itself the leak.
#[test]
fn repeated_snapshots_do_not_accumulate() -> Result<(), Box<dyn Error>> {
    let (runtime, mut supervisor) = supervisor(2)?;
    runtime.block_on(async {
        let gate = Gate::closed();
        spawn_gated(&mut supervisor, &gate, 1).await;
        let mut sizes: Vec<usize> = Vec::new();
        for _ in 0..64 {
            sizes.push(supervisor.snapshot().live().len());
        }
        assert!(
            sizes.iter().all(|size| *size == 1),
            "sixty-four reads of a one-task supervisor all report one task: {sizes:?}"
        );
        gate.release(1);
        gate.await_exits(1).await;
        reap_until_completed(&mut supervisor, 1).await;
        let report = supervisor.shutdown().await;
        assert_eq!(report.stats().succeeded, 1, "the one body completed");
    });
    Ok(())
}

/// The snapshot never starts, stops or reschedules anything. A read that had a
/// side effect could not be used from a monitoring loop without changing what it
/// was monitoring.
#[test]
fn reading_a_snapshot_has_no_side_effect_on_admission() -> Result<(), Box<dyn Error>> {
    let (runtime, mut supervisor) = supervisor(2)?;
    runtime.block_on(async {
        // Fill one slot; read the snapshot many times; then prove the second slot
        // is still genuinely available by taking it.
        let gate = Gate::closed();
        spawn_gated(&mut supervisor, &gate, 1).await;
        for _ in 0..32 {
            assert_eq!(
                supervisor.snapshot().free(),
                1,
                "thirty-two reads did not consume or release a permit"
            );
        }
        let gate2 = Gate::closed();
        spawn_gated(&mut supervisor, &gate2, 1).await;
        assert_eq!(
            supervisor.snapshot().occupied(),
            2,
            "the permit the reads left alone was still there to be taken"
        );
        gate.release(1);
        gate2.release(1);
        gate.await_exits(1).await;
        gate2.await_exits(1).await;
        reap_until_completed(&mut supervisor, 2).await;
        let report = supervisor.shutdown().await;
        assert_eq!(report.stats().succeeded, 2, "both bodies completed");
    });
    Ok(())
}

/// A deadline that stops a repeating body is reported as exhausted work, and the
/// snapshot's counters stay consistent with the drained record. This ties the
/// clock contract to the inspection contract: the timer that bounds the work is
/// the engine's, and the report says which way the loop ended.
#[test]
fn a_bounded_repeating_task_reports_exhaustion_not_a_hang() -> Result<(), Box<dyn Error>> {
    use lgwks_bot::rt::supervise::Budget;

    let (runtime, mut supervisor) = supervisor(2)?;
    runtime.block_on(async {
        let iterations =
            std::num::NonZeroU64::new(4).ok_or("a repeating budget of zero iterations")?;
        let ran = Arc::new(AtomicU64::new(0));
        let ran_clone = Arc::clone(&ran);
        supervisor
            .spawn_repeating(Budget::Iterations(iterations), move |tick| {
                let ran = Arc::clone(&ran_clone);
                async move {
                    let _ = tick;
                    ran.fetch_add(1, Ordering::SeqCst);
                    sleep(Duration::from_millis(1)).await;
                }
            })
            .await;
        // Wait for the loop to finish its budget before draining. `shutdown`
        // cancels first, so draining early would report a cancelled loop and the
        // test would be measuring the shutdown rather than the exhaustion.
        let expected = iterations.get();
        let watch = Arc::clone(&ran);
        await_until(move || watch.load(Ordering::SeqCst) >= expected).await;
        // `stats().completed` advances only on a reap, so drive one. This is the
        // fact the assertion below turns on: the loop's outcome is accounted as
        // a completion, which requires the terminal join to have happened.
        reap_until_completed(&mut supervisor, 1).await;
        let report = supervisor.shutdown().await;
        assert_eq!(
            ran.load(Ordering::SeqCst),
            expected,
            "the loop ran exactly the iterations its budget allowed, so it \\
             stopped by exhausting the budget rather than by hanging or by \\
             being cancelled"
        );
        assert_eq!(
            report.stats().succeeded,
            1,
            "a loop that spent its iteration budget completed its work, so this \\
             is not reported as a hang or as a cancellation"
        );
        assert_eq!(report.stats().cancelled, 0, "nothing cancelled the loop");
        assert!(report.is_clean(), "an exhausted loop is a clean drain");
        Ok::<_, Box<dyn Error>>(())
    })?;
    Ok(())
}
