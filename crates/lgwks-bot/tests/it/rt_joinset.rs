//! Direct witnesses for the `rt::task::JoinSet` ownership wrapper (#415).
//!
//! The wrapper's whole contract is three sentences in `rt/task.rs`: a task
//! spawned into the set is joined through it, dropping the set aborts what has
//! not finished, and the engine's `detach_all` is not reachable through it.
//! Source: tokio's `JoinSet` documentation ("When the `JoinSet` is dropped, all
//! tasks in the `JoinSet` are immediately aborted") and its `detach_all`, which
//! is the one engine method that lets a task outlive its owner.
//!
//! No test here sleeps or races a wall clock. The abort is observed through the
//! task's own future being dropped (a guard's `Drop` closes a oneshot), which
//! the engine does only when it cancels the task; a detaching set never drops
//! it, so that arm would park, and the outer `timeout` turns the park into a
//! named failure instead of a hung run.
#![cfg(all(feature = "rt", feature = "sync", feature = "time"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::sync::oneshot;
use lgwks_bot::rt::task::JoinSet;
use lgwks_bot::rt::time::timeout;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A ceiling on a wait that only a defect can reach; never a pacing sleep.
const PARK_CEILING: Duration = Duration::from_secs(10);

/// Closes its sender when the future holding it is dropped, which is how the
/// engine's abort becomes visible to the test without a timer.
struct DropSignal(Option<oneshot::Sender<()>>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        // Dropping the sender is the signal; the receiver reads `Err` either way.
        drop(self.0.take());
    }
}

/// Spawn `tasks` into one set at once and join every one to completion.
///
/// Returns the sum of the joined values and the number of joins, so the caller
/// can assert that each task was joined exactly once and the set ended empty.
fn spawn_and_join(tasks: u64) -> Result<(u64, u64, usize, bool), Box<dyn std::error::Error>> {
    let runtime = Runtime::new()?;
    runtime.block_on(async move {
        let mut set = JoinSet::new();
        for value in 0..tasks {
            set.spawn(async move { value });
        }
        let owned = set.len();
        let mut sum = 0u64;
        let mut joins = 0u64;
        while let Some(joined) = set.join_next().await {
            sum = sum.saturating_add(joined?);
            joins = joins.saturating_add(1);
        }
        Ok((sum, joins, owned, set.is_empty()))
    })
}

#[test]
fn spawn_and_join_next_drive_every_tier_to_completion() -> TestResult {
    for tasks in [100u64, 1_000, 10_000, 100_000] {
        let (sum, joins, owned, empty) = spawn_and_join(tasks)?;
        assert_eq!(owned, usize::try_from(tasks)?, "the set owns all {tasks}");
        assert_eq!(joins, tasks, "each of {tasks} tasks joins exactly once");
        let expected = (0..tasks).fold(0u64, u64::saturating_add);
        assert_eq!(sum, expected, "every value at tier {tasks}");
        assert!(empty, "a drained set at tier {tasks} owns nothing");
    }
    Ok(())
}

#[test]
fn dropping_the_set_aborts_a_task_that_never_resolves() -> TestResult {
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (dropped_tx, dropped_rx) = oneshot::channel::<()>();
        let mut set = JoinSet::<()>::new();
        set.spawn(async move {
            let _guard = DropSignal(Some(dropped_tx));
            // A closed receiver means the test already gave up; the guard
            // still drops on the way out, so nothing is lost by returning.
            if started_tx.send(()).is_err() {
                return;
            }
            std::future::pending::<()>().await;
        });
        // The body is running and parked before the drop, so the drop is what
        // ends it, not an abort of a task that never started.
        timeout(PARK_CEILING, started_rx).await??;
        drop(set);
        match timeout(PARK_CEILING, dropped_rx).await {
            Ok(Err(_closed)) => Ok(()),
            Ok(Ok(())) => Err("the guard never sends; only its drop closes".into()),
            Err(_elapsed) => Err("the task outlived its dropped set: it was detached".into()),
        }
    })
}

#[test]
fn abort_all_cancels_every_owned_task_and_try_join_drains_them() -> TestResult {
    let runtime = Runtime::new()?;
    let finished = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&finished);
    runtime.block_on(async move {
        let mut set = JoinSet::<()>::new();
        assert!(set.try_join_next().is_none(), "an empty set yields nothing");
        for _ in 0..64 {
            let counter = Arc::clone(&counter);
            set.spawn(async move {
                std::future::pending::<()>().await;
                counter.fetch_add(1, SeqCst);
            });
        }
        set.abort_all();
        let mut cancelled = 0usize;
        while let Some(joined) = set.join_next().await {
            let Err(error) = joined else {
                return Err("a pending task cannot complete".into());
            };
            assert!(error.is_cancelled(), "abort_all is cancellation: {error}");
            cancelled += 1;
        }
        assert_eq!(cancelled, 64, "every owned task reports its cancellation");
        assert!(set.is_empty() && set.try_join_next_with_id().is_none());
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;
    assert_eq!(
        finished.load(SeqCst),
        0,
        "no aborted body ran past its park"
    );
    Ok(())
}

/// Drain one finished task through the non-waiting door, yielding the worker
/// between tries so the engine can record the completion; the park ceiling only
/// bounds a defect, so a door that never yields fails by name instead of hanging.
async fn try_drain<R>(mut attempt: impl FnMut() -> Option<R>) -> Result<R, String> {
    let drained = timeout(PARK_CEILING, async {
        loop {
            if let Some(joined) = attempt() {
                return joined;
            }
            lgwks_bot::rt::task::yield_now().await;
        }
    });
    drained
        .await
        .map_err(|elapsed| format!("try_join_next never yielded a finished task: {elapsed}"))
}

#[test]
fn the_non_waiting_doors_yield_a_finished_task_and_its_identity() -> TestResult {
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut set = JoinSet::new();
        set.spawn(async { 3u8 });
        assert!(!set.is_empty() && set.len() == 1, "the set owns its task");
        let value = try_drain(|| set.try_join_next()).await??;
        assert_eq!(value, 3);
        assert!(set.is_empty(), "the drained task is no longer owned");

        let handle = set.spawn(async { 4u8 });
        let (id, value) = try_drain(|| set.try_join_next_with_id()).await??;
        assert_eq!((id, value), (handle.id(), 4));
        Ok(())
    })
}

#[test]
fn join_next_with_id_names_the_task_its_abort_handle_names() -> TestResult {
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut set = JoinSet::new();
        let handle = set.spawn(async { 7u8 });
        let Some(joined) = set.join_next_with_id().await else {
            return Err("the set yields the one task it holds".into());
        };
        let (id, value) = joined?;
        assert_eq!((id, value), (handle.id(), 7));
        assert!(set.join_next_with_id().await.is_none());
        Ok(())
    })
}

/// The consumer probe for `detach_all`: the engine method exists on tokio's
/// set, so only the wrapper's narrowing makes this a compile error.
#[cfg(all(unix, feature = "process"))]
mod detach_probe {
    use crate::compile::{assert_refused_for, compile_probe};

    const DEPENDENCY: &str = ", features = [\"rt\", \"sync\"]";

    fn consumer(call: &str) -> String {
        format!(
            "fn main() {{\n    let mut set = lgwks_bot::rt::task::JoinSet::<()>::new();\n    {call}\n}}\n"
        )
    }

    #[test]
    fn detach_all_is_unreachable_through_the_wrapper() -> Result<(), Box<dyn std::error::Error>> {
        let control = compile_probe("joinset_control", DEPENDENCY, &consumer("set.abort_all();"))?;
        assert!(
            control.status.success(),
            "the positive control must compile:\n{}",
            String::from_utf8_lossy(&control.stderr)
        );
        let refused = compile_probe("joinset_detach", DEPENDENCY, &consumer("set.detach_all();"))?;
        assert_refused_for(&refused, "E0599", "detach_all");
        Ok(())
    }
}
