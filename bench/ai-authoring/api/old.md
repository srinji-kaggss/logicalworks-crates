# API sheet — the old surface: `lgwks_bot::rt`

You may use only the items on this sheet plus `std` and `ai_task_support`.

This is the low-level async surface. Every item below is a real public item of
the crate. Signatures are given exactly as they are declared.

## Entering async

- `lgwks_bot::rt::runtime::Runtime` — an owned async runtime. Dropping it shuts
  it down and aborts unfinished tasks.
  - `fn new() -> std::io::Result<Runtime>`
  - `fn block_on<F: Future>(&self, future: F) -> F::Output`
  - `fn handle(&self) -> Handle`
  - `fn shutdown_timeout(self, timeout: std::time::Duration)`
- `lgwks_bot::rt::runtime::block_on<F: Future>(future: F) -> F::Output` — build a
  current-thread runtime, drive one future, tear it down.
- `Handle` — cloneable; `fn block_on<F: Future>(&self, future: F) -> F::Output`.

## Bounded fan-out

- `lgwks_bot::rt::task::join_all_bounded`
  ```rust
  pub async fn join_all_bounded<F, T>(
      limit: usize,
      futures: impl IntoIterator<Item = F>,
  ) -> Vec<T>
  where
      F: Future<Output = T> + Send + 'static,
      T: Send + 'static;
  ```
  Runs every future, never more than `limit` at once, and returns their outputs
  in **input order**. Dropping the returned future aborts every unfinished task.
  It does **not** stop on the first error: inspect the results yourself.
- `lgwks_bot::rt::task::JoinSet<T>` — a task set that owns what it starts.
  - `fn new() -> JoinSet<T>`
  - `fn spawn<F>(&mut self, future: F) -> AbortHandle`
    where `F: Future<Output = T> + Send + 'static, T: 'static`
  - `async fn join_next(&mut self) -> Option<Result<T, JoinError>>`
  - `fn try_join_next(&mut self) -> Option<Result<T, JoinError>>`
  - `fn abort_all(&mut self)`
  - `fn is_empty(&self) -> bool`, `fn len(&self) -> usize`
  Dropping the set aborts its remaining tasks.
- `JoinError`, `AbortHandle`, and `async fn yield_now()`.

## Supervision

- `lgwks_bot::rt::supervise::Supervisor` — a bounded set of tasks that reports
  how each ended and stops them when dropped.
  - `fn new(max_in_flight: usize) -> Supervisor`
  - `fn with_clock(max_in_flight: usize, clock: Clock) -> Supervisor`
  - `async fn spawn<F, Fut>(&mut self, body: F)`
    where `F: FnOnce(CancellationToken) -> Fut, Fut: Future<Output = ()> + Send + 'static`
  - `fn try_spawn<F, Fut>(&mut self, body: F) -> Result<(), TrySpawnRefusal>`
  - `fn cancel(&self)`, `fn is_cancelled(&self) -> bool`
  - `fn child_token(&self) -> CancellationToken`
  - `fn stats(&self) -> Stats`
  - `fn next_report(&mut self) -> Option<TaskOutcome>`
  - `fn reap(&mut self) -> usize`
  - `fn snapshot(&self) -> SupervisorSnapshot`
  - `async fn shutdown(self) -> ShutdownReport`
- `Stats` — public fields `spawned, completed, succeeded, failed, cancelled,
  aborted, panicked, refused, reports_dropped` (all `u64`), and
  `fn in_flight(self) -> u64`.
- `TaskOutcome` — `Completed { task, cleanup }`, `Cancelled { task, cleanup }`,
  `Aborted { task }`, `Panicked { task, message }`, `Failed { task, status, cleanup }`;
  `fn is_success(&self) -> bool`, `fn is_panic(&self) -> bool`.
- `ShutdownReport` — `fn outcomes(&self) -> &[TaskOutcome]`,
  `fn is_clean(&self) -> bool`, `fn stats(&self) -> Stats`.
- `Budget { Iterations(NonZeroU64), For(Duration), Ongoing }` and
  `Outcome { Exhausted { iterations }, Cancelled { iterations } }`;
  `async fn repeat<F, Fut>(token: &CancellationToken, budget: Budget, body: F) -> Outcome`
  where `F: FnMut(u64) -> Fut, Fut: Future<Output = ()>`.

## Cancellation

- `lgwks_bot::rt::cancel::CancellationToken`
  - `fn new() -> CancellationToken`
  - `fn cancel(&self)`, `fn is_cancelled(&self) -> bool`
  - `async fn cancelled(&self)`
  - `fn cancelled_owned(&self) -> impl Future<Output = ()> + Send + 'static`
  - `fn child_token(&self) -> CancellationToken`
  - `fn drop_guard(self) -> DropGuard`
  - `async fn run_until_cancelled<F: Future>(&self, future: F) -> Option<F::Output>`

## Time

- `lgwks_bot::rt::time::sleep(duration: std::time::Duration)` —
  `async fn`; the deadline is captured on first poll.
- `lgwks_bot::rt::time::timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed>`
- `lgwks_bot::rt::time::{timeout_at, sleep_until, Instant}`.

## Sync

- `lgwks_bot::rt::sync::{mpsc, oneshot, broadcast, watch, Mutex, RwLock,
  Semaphore, Notify, Barrier}` — the engine's bounded channels and async locks,
  re-exported. Use `Semaphore` for a hand-built admission bound.

## A bounded fan-out in this API

```rust
use lgwks_bot::rt::runtime::Runtime;
use lgwks_bot::rt::task::join_all_bounded;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Runtime::new()?;
    let total: u64 = runtime.block_on(async {
        let futures = (0u64..100).map(|value| async move { value * 2 });
        join_all_bounded(8, futures).await.into_iter().sum()
    });
    assert_eq!(total, 9900, "0..100 doubled");
    Ok(())
}
```

## Notes

- There is no `spawn` that returns a droppable handle to a running task.
  `JoinSet` owns what it starts, and dropping the set aborts.
- `join_all_bounded` bounds concurrency but does not fail fast: to stop on the
  first failure you must start work in bounded waves and check each wave.
