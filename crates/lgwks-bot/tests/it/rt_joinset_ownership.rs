//! Direct witnesses for the `JoinSet` ownership contract (#415).
//!
//! `lgwks_bot::rt::task::JoinSet` documents three promises the Supervisor
//! builds on: dropping the set aborts every task it still owns, `abort_all`
//! cancels what has not finished, and there is no `detach_all` to remove a
//! task without aborting it. Tokio's own `JoinSet` carries the same drop
//! semantics ("when the JoinSet is dropped, all tasks in the JoinSet are
//! immediately aborted", tokio::task::JoinSet docs); this file proves the
//! wrapper keeps them rather than assuming the engine does. The missing
//! `detach_all` is proved by a `compile_fail` doctest on the type itself.
//!
//! Each property is driven through the public surface at 100, 1,000 and
//! 10,000 concurrently owned tasks. A pending task holds a guard whose drop
//! is counted, so an abort is observed as the task's future being dropped,
//! not inferred from a status. Every wait is on an event (the last guard's
//! drop cancels a token), bounded by a ceiling that fails the test instead of
//! hanging it.
#![cfg(all(feature = "rt", feature = "time", feature = "sync"))]

use std::future::{Future, pending};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::sync::CancellationToken;
use lgwks_bot::rt::task::{AbortHandle, JoinSet, yield_now};
use lgwks_bot::rt::time::timeout;

/// Every test returns its failure with a reason instead of unwinding.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The concurrency tiers every property is held at.
const TIERS: [usize; 3] = [100, 1_000, 10_000];

/// The longest any wait here may take before it is a failure. Each one ends
/// on an event long before it on a working set; the ceiling exists only so a
/// set that never aborts fails with a reason instead of hanging the run.
const CEILING: Duration = Duration::from_secs(30);

/// The task whose own `AbortHandle` is used, an index every tier contains.
const VICTIM: usize = 37;

/// Counts its own drop, and cancels `all` when it is the `of`-th to go.
struct DropWitness {
    /// Drops observed so far, shared by every witness in one tier.
    dropped: Arc<AtomicUsize>,
    /// How many witnesses the tier created.
    of: usize,
    /// Cancelled by the last witness to drop.
    all: CancellationToken,
}

impl Drop for DropWitness {
    fn drop(&mut self) {
        if self.dropped.fetch_add(1, SeqCst).saturating_add(1) == self.of {
            self.all.cancel();
        }
    }
}

/// `count` witnesses sharing one counter and one "all dropped" token.
fn witnesses(count: usize) -> (Vec<DropWitness>, Arc<AtomicUsize>, CancellationToken) {
    let dropped = Arc::new(AtomicUsize::new(0));
    let all = CancellationToken::new();
    let made = (0..count)
        .map(|_| DropWitness {
            dropped: Arc::clone(&dropped),
            of: count,
            all: all.clone(),
        })
        .collect();
    (made, dropped, all)
}

/// A task that holds `witness` and never finishes on its own.
async fn held_forever(witness: DropWitness) -> usize {
    let _held = witness;
    pending::<usize>().await
}

/// Starts `count` tasks that each finish with their index once `gate` opens,
/// returning their abort handles in index order.
fn spawn_gated(
    set: &mut JoinSet<usize>,
    count: usize,
    gate: &CancellationToken,
) -> Vec<AbortHandle> {
    (0..count)
        .map(|index| {
            let open = gate.clone();
            set.spawn(async move {
                open.cancelled().await;
                index
            })
        })
        .collect()
}

/// Joins `set` to empty: the finished values and the count of cancelled
/// tasks. A task that failed any other way is the test's failure.
async fn drain(
    set: &mut JoinSet<usize>,
    tier: usize,
) -> Result<(Vec<usize>, usize), Box<dyn std::error::Error>> {
    let (mut values, mut cancelled) = (Vec::new(), 0_usize);
    while let Some(joined) = timeout(CEILING, set.join_next()).await? {
        match joined {
            Ok(value) => values.push(value),
            Err(error) if error.is_cancelled() => cancelled = cancelled.saturating_add(1),
            Err(error) => {
                return Err(format!(
                    "tier {tier}: a task failed rather than finishing or being cancelled: {error}"
                )
                .into());
            }
        }
    }
    values.sort_unstable();
    Ok((values, cancelled))
}

/// The next task `attempt` (a non-waiting join) yields, or `None` once the set
/// is empty.
///
/// Between empty answers it yields to the scheduler so the finishing tasks
/// run: the wait ends on their completion, never on a timer, and `CEILING`
/// only turns a set that never yields into a failure.
async fn until_ready<R>(
    set: &mut JoinSet<usize>,
    attempt: fn(&mut JoinSet<usize>) -> Option<R>,
) -> Result<Option<R>, Box<dyn std::error::Error>> {
    let next = timeout(CEILING, async {
        loop {
            if set.is_empty() {
                return None;
            }
            if let Some(ready) = attempt(set) {
                return Some(ready);
            }
            yield_now().await;
        }
    });
    Ok(next.await?)
}

/// Runs `check` once per tier on one runtime, failing on the first tier whose
/// check fails.
fn tiers<F, Fut>(check: F) -> TestResult
where
    F: Fn(usize) -> Fut,
    Fut: Future<Output = TestResult>,
{
    let runtime = Runtime::new()?;
    for tier in TIERS {
        runtime.block_on(check(tier))?;
    }
    Ok(())
}

/// [`tiers`] on a set already holding `tier` gated tasks (see
/// [`spawn_gated`]): `check` gets the set, the tasks' abort handles in index
/// order, and the gate that lets them finish.
fn gated_tiers<F, Fut>(check: F) -> TestResult
where
    F: Fn(usize, JoinSet<usize>, Vec<AbortHandle>, CancellationToken) -> Fut,
    Fut: Future<Output = TestResult>,
{
    // The spawn happens inside the future, because a task can only be started
    // on a running runtime.
    let check = &check;
    tiers(move |tier| async move {
        let gate = CancellationToken::new();
        let mut set = JoinSet::new();
        let handles = spawn_gated(&mut set, tier, &gate);
        check(tier, set, handles, gate).await
    })
}

#[test]
/// Dropping the set aborts every task it still owns: each pending task's
/// future is dropped, which is what releases what the task held.
fn dropping_the_set_aborts_every_owned_task() -> TestResult {
    tiers(|tier| async move {
        let (made, dropped, all) = witnesses(tier);
        let mut set = JoinSet::new();
        for witness in made {
            set.spawn(held_forever(witness));
        }
        assert_eq!(set.len(), tier, "the set owns every task it started");
        drop(set);
        timeout(CEILING, all.cancelled()).await.map_err(|_| {
            format!(
                "tier {tier}: only {} of {tier} tasks were aborted by the drop",
                dropped.load(SeqCst)
            )
        })?;
        assert_eq!(
            dropped.load(SeqCst),
            tier,
            "tier {tier}: every owned task was aborted"
        );
        Ok(())
    })
}

#[test]
/// `abort_all` cancels every unfinished task, each joins as cancelled rather
/// than as a value or a panic, and a finished task keeps its value.
fn abort_all_cancels_every_unfinished_task_and_keeps_finished_values() -> TestResult {
    tiers(|tier| async move {
        // Every third task finishes with its index; the rest hold forever.
        let held_count = tier.saturating_sub(tier.div_ceil(3));
        let (made, dropped, all) = witnesses(held_count);
        let mut held = made.into_iter();
        let mut set = JoinSet::new();
        for index in 0..tier {
            if index % 3 == 0 {
                set.spawn(async move { index });
            } else {
                let witness = held.next().ok_or("one witness per held task")?;
                set.spawn(held_forever(witness));
            }
        }
        set.abort_all();
        let (values, cancelled) = drain(&mut set, tier).await?;
        assert!(set.is_empty(), "tier {tier}: joining drains the set");
        assert_eq!(
            dropped.load(SeqCst),
            held_count,
            "tier {tier}: every held task was dropped"
        );
        assert!(
            all.is_cancelled(),
            "tier {tier}: the last held task's drop was observed"
        );
        // A task that finished before the abort reached it keeps its value;
        // every other one is cancelled. Nothing is lost or doubled.
        assert_eq!(
            values.len().saturating_add(cancelled),
            tier,
            "tier {tier}: every task joins exactly once"
        );
        assert!(
            cancelled >= held_count,
            "tier {tier}: no held task produced a value"
        );
        assert!(
            values.iter().all(|value| value % 3 == 0),
            "tier {tier}: only finishing tasks yield values"
        );
        Ok(())
    })
}

#[test]
/// One task's `AbortHandle` cancels that task and no other.
fn an_abort_handle_cancels_only_its_own_task() -> TestResult {
    gated_tiers(|tier, mut set, handles, gate| async move {
        handles
            .get(VICTIM)
            .ok_or("the victim task's handle")?
            .abort();
        gate.cancel();
        let (values, cancelled) = drain(&mut set, tier).await?;
        let expected: Vec<usize> = (0..tier).filter(|&index| index != VICTIM).collect();
        assert_eq!(
            cancelled, 1,
            "tier {tier}: exactly the aborted task is cancelled"
        );
        assert_eq!(
            values, expected,
            "tier {tier}: every other task finishes with its value"
        );
        Ok(())
    })
}

#[test]
/// `try_join_next` never waits: it answers `None` while nothing has finished,
/// and the set then yields each task exactly once.
fn try_join_next_answers_without_waiting() -> TestResult {
    gated_tiers(|tier, mut set, _handles, gate| async move {
        assert!(
            set.try_join_next().is_none(),
            "tier {tier}: nothing has finished yet"
        );
        gate.cancel();
        // Drained through the non-waiting join alone, so each finished task
        // must come out of `try_join_next` itself.
        let mut values = Vec::with_capacity(tier);
        while let Some(joined) = until_ready(&mut set, JoinSet::try_join_next).await? {
            values.push(joined?);
        }
        values.sort_unstable();
        assert!(
            set.try_join_next().is_none(),
            "tier {tier}: a drained set yields nothing"
        );
        assert_eq!(
            values,
            (0..tier).collect::<Vec<_>>(),
            "tier {tier}: each task joins once with its value"
        );
        Ok(())
    })
}

#[test]
/// The `_with_id` joins report the same identity as the task's own
/// `AbortHandle`, once per task, so a caller can match an outcome to the work
/// it started; the non-waiting form answers `None` until a task finishes.
fn joins_with_id_match_each_tasks_abort_handle() -> TestResult {
    gated_tiers(|tier, mut set, handles, gate| async move {
        assert!(
            !set.is_empty(),
            "tier {tier}: a set holding tasks is not empty"
        );
        assert!(
            set.try_join_next_with_id().is_none(),
            "tier {tier}: nothing has finished yet"
        );
        gate.cancel();
        let mut seen = vec![false; tier];
        // Alternate the waiting and the non-waiting join, so both must report
        // each task's identity.
        let mut wait = true;
        loop {
            let next = if wait {
                timeout(CEILING, set.join_next_with_id()).await?
            } else {
                until_ready(&mut set, JoinSet::try_join_next_with_id).await?
            };
            let Some(joined) = next else { break };
            wait = !wait;
            let (id, index) = joined?;
            let handle = handles
                .get(index)
                .ok_or("a joined index the tier started")?;
            assert_eq!(
                id,
                handle.id(),
                "tier {tier}: task {index} joins under its own identity"
            );
            let slot = seen
                .get_mut(index)
                .ok_or("a joined index the tier started")?;
            assert!(!*slot, "tier {tier}: task {index} joined twice");
            *slot = true;
        }
        assert!(
            seen.iter().all(|&joined| joined),
            "tier {tier}: every task joined"
        );
        assert!(
            set.try_join_next_with_id().is_none(),
            "tier {tier}: a drained set yields nothing"
        );
        Ok(())
    })
}
